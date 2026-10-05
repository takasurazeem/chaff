import Foundation
import Observation
import chaff_ffiFFI

/// The culling loop: what a keystroke does.
///
/// # Why this is its own type and not methods on the view
///
/// Rating, rejecting and undoing are the operations a mistake loses work with, and they are
/// set arithmetic plus a stack — the kind of code that is wrong in a way nobody notices until
/// it has eaten an afternoon of ratings. Kept out of the view so it can be reasoned about, and
/// because the web app's equivalent had a bug that only a unit test found.
///
/// # The undo stack covers two kinds of action
///
/// A rating is reversed by **writing the previous value back**. A move is not: the files are in
/// the trash and undoing it means **restoring the operation**, which is a different command
/// entirely. Modelling them as one shape would mean one of the two being a lie.
@MainActor
@Observable
final class Culling {
    /// The photograph the keyboard acts on.
    ///
    /// Separate from the selection because they answer different questions: the selection is
    /// what a delete would move, the cursor is what `3` rates. Collapsing them means rating
    /// four photographs at once, which is never what someone pressing `3` means.
    var cursor: Int64?

    private(set) var undoDepth = 0
    private var undoStack: [Action] = []
    private let limit = 500

    private enum Action {
        case decision(photoId: Int64, previous: Decision)
        case trash(opId: String, label: String)
    }

    private struct Decision {
        let rating: UInt8
        let rejected: Bool
    }

    /// Rate the photograph under the cursor.
    func rate(_ rating: UInt8, in model: EngineModel) async {
        guard let id = cursor, let photo = model.photos.first(where: { $0.id == id }) else { return }
        await apply(id: id, rating: rating, rejected: photo.rejected, in: model)
    }

    /// Reject, or un-reject.
    func toggleReject(in model: EngineModel) async {
        guard let id = cursor, let photo = model.photos.first(where: { $0.id == id }) else { return }
        // Rejecting does **not** clear the rating: a rejected photograph keeps the stars it
        // had, so un-rejecting restores what the user thought of it.
        await apply(id: id, rating: photo.rating, rejected: !photo.rejected, in: model)
    }

    /// Write a decision, remembering what it was.
    private func apply(id: Int64, rating: UInt8, rejected: Bool, in model: EngineModel) async {
        guard let photo = model.photos.first(where: { $0.id == id }) else { return }
        let previous = Decision(rating: photo.rating, rejected: photo.rejected)

        do {
            try await model.setDecision(photoId: id, rating: rating, rejected: rejected)
            push(.decision(photoId: id, previous: previous))
            model.applyLocally(photoId: id, rating: rating, rejected: rejected)
        } catch {
            model.errorMessage = model.describe(error)
        }
    }

    /// Record a move, so it can be undone.
    ///
    /// Called after the engine has committed — the operation id only exists once it has.
    func recordTrash(opId: String, moved: UInt32) {
        guard moved > 0 else { return }
        push(.trash(opId: opId, label: "move \(moved) file\(moved == 1 ? "" : "s") to trash"))
    }

    private func push(_ action: Action) {
        undoStack.append(action)
        // Bounded, like the web app's: a long session should not accumulate closures forever.
        if undoStack.count > limit {
            undoStack.removeFirst(undoStack.count - limit)
        }
        undoDepth = undoStack.count
    }

    /// Reverse the last action.
    func undo(in model: EngineModel) async {
        guard let action = undoStack.popLast() else { return }
        undoDepth = undoStack.count

        do {
            switch action {
            case let .decision(photoId, previous):
                try await model.setDecision(
                    photoId: photoId, rating: previous.rating, rejected: previous.rejected
                )
                model.applyLocally(photoId: photoId, rating: previous.rating, rejected: previous.rejected)

            case let .trash(opId, _):
                // **A move is undone by restoring the operation.** The files come back and the
                // photographs reappear, which is a different thing from writing a value back.
                try await model.restoreTrash(opId: opId)
            }
        } catch {
            model.errorMessage = model.describe(error)
        }
    }
}
