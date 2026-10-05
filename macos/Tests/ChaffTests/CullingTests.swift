import Testing
import chaff_ffiFFI
@testable import Chaff

/// The culling state machine.
///
/// `Culling` holds the cursor and the undo stack. The engine calls inside `rate` and `undo` are
/// not exercised here — those need an `EngineModel`, which needs a catalog, and a test that
/// builds one is testing the engine rather than this type. What *is* testable without any of
/// that is the state the type owns, and that is where the bugs that eat an afternoon live.
@MainActor
struct CullingTests {
    @Test("The undo depth starts at zero and reports what it holds")
    func undoStartsEmpty() {
        let culling = Culling()
        #expect(culling.undoDepth == 0)
        #expect(culling.cursor == nil)
    }

    @Test("Recording a move that moved nothing records nothing")
    func anEmptyMoveIsNotUndoable() {
        // A delete that moved zero files has nothing to reverse, and pushing it would make ⌘Z
        // do nothing visible — which reads as undo being broken rather than as there being
        // nothing to undo.
        let culling = Culling()
        culling.recordTrash(opId: "op-1", moved: 0)
        #expect(culling.undoDepth == 0, "a move of zero files must not become an undo step")
    }

    @Test("Recording a real move adds exactly one step, whatever the file count")
    func oneMoveIsOneStep() {
        // **One operation is one undo**, not one per file. Ten steps for one delete means ⌘Z
        // ten times to reverse a single action.
        let culling = Culling()
        culling.recordTrash(opId: "op-1", moved: 1)
        #expect(culling.undoDepth == 1)
        culling.recordTrash(opId: "op-2", moved: 47)
        #expect(culling.undoDepth == 2)
    }

    @Test("The undo stack is bounded, so a long session does not grow forever")
    func undoIsBounded() {
        // The web app bounds its stack at 500 for the same reason. A culling session is
        // thousands of keystrokes, and an unbounded stack is a leak that only shows on the
        // session that matters.
        let culling = Culling()
        for i in 0..<700 {
            culling.recordTrash(opId: "op-\(i)", moved: 1)
        }
        #expect(culling.undoDepth == 500, "the stack must be capped at its limit")
    }

    @Test("The cursor is independent of anything else the type holds")
    func theCursorIsItsOwnState() {
        // The cursor is what `3` rates and the selection is what a delete would move. They are
        // deliberately separate — collapsing them means rating four photographs at once — and
        // this asserts the cursor is settable on its own.
        let culling = Culling()
        culling.cursor = 42
        #expect(culling.cursor == 42)
        culling.cursor = nil
        #expect(culling.cursor == nil)
    }
}


/// What the error alert shows.
///
/// `describe` is the only thing between an engine error and a sentence in a dialog, and it is
/// now displayed in eight places — so a message that reads badly is a message the user reads
/// eight times.
@MainActor
struct DescribeTests {
    @Test("A refusal reads as an instruction, not a stack trace")
    func refusalIsActionable() {
        let model = EngineModel()
        let message = model.describe(
            ChaffError.Engine(
                kind: .refused,
                message: "That is the same group — pick a different one to merge into."
            )
        )
        // **The engine's own sentence is the good one**, so it must survive. A `describe` that
        // replaced it with "an error occurred" would throw away the only useful part.
        #expect(message.contains("pick a different one"))
        #expect(!message.lowercased().contains("error:"))
    }

    @Test("Every failure kind produces something a person can read")
    func everyKindReads() {
        let model = EngineModel()
        // A table over the kinds, because the failure mode is one arm falling through to a
        // debug string — and `Other` is exactly where that would hide.
        let kinds: [FailureKind] = [.busy, .notFound, .poisoned, .refused, .other]
        for kind in kinds {
            let message = model.describe(ChaffError.Engine(kind: kind, message: "the detail"))
            #expect(!message.isEmpty, "\(kind) produced an empty message")
            #expect(
                message.contains("the detail"),
                "\(kind) dropped the engine's own detail, which is the part that helps"
            )
        }
    }

    @Test("A missing file says which file")
    func notFoundNamesTheThing() {
        let model = EngineModel()
        let message = model.describe(
            ChaffError.Engine(kind: .notFound, message: "IMG_0133.CR3 is no longer there")
        )
        #expect(message.contains("IMG_0133.CR3"), "a not-found that does not name the file is useless")
    }
}
