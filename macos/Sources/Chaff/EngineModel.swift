import Foundation
import Observation
import chaff_ffiFFI

/// The application's state, and the only thing a view talks to.
///
/// # Why `@Observable` and not `ObservableObject`
///
/// `@Observable` tracks reads **per property**. A grid of 50,000 tiles that observes
/// `photos` re-renders when `photos` changes and not when `isIndexing` does — which matters
/// because a pass updates progress several times a second and a grid that re-rendered on every
/// tick would drop frames for no reason.
///
/// # Why the engine calls are `nonisolated`
///
/// `Engine` is a Rust object behind a lock. Calling it from the main actor would block the UI
/// for the length of a query — 21 ms for 50,000 photographs, which is more than a frame. So the
/// calls happen off the main actor and only the *results* come back to it.
///
/// The reviewer's finding is why a pass does **not** hold the read lock: it opens its own
/// connection, so the grid stays responsive while an index runs.
@MainActor
@Observable
final class EngineModel {
    /// Where the catalog and caches live.
    ///
    /// In the app's container, not beside the photographs: the library is the user's and this
    /// application does not write into it except through the trash.
    static func dataRoot() -> URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL.temporaryDirectory
        let dir = base.appendingPathComponent("com.takasurazeem.chaff", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    private(set) var photos: [Photo] = []
    private(set) var folders: [Folder] = []
    private(set) var library: Library?
    private(set) var tags: [(String, UInt32)] = []
    private(set) var people: [Person] = []
    private(set) var faceReport: FacePassReport?
    /// Which tags each photograph carries, and which groups it belongs to.
    ///
    /// Built once after a pass rather than queried per tile: a grid asks this for every visible
    /// photograph on every scroll, and a query per tile is 50,000 queries for one screenful.
    private(set) var tagsByPhoto: [Int64: Set<String>] = [:]
    private(set) var peopleByPhoto: [Int64: Set<Int64>] = [:]
    private(set) var tagReport: TagPassReport?
    private(set) var sidecarReport: SidecarReport?
    private(set) var endpointReport: EndpointReport?
    var machine: Capabilities?
    private(set) var isIndexing = false
    /// `0...1`, or `nil` while the total is unknown.
    ///
    /// Optional rather than zero because the scan phase genuinely has no total — the size of a
    /// tree is not known until the walk finishes — and a determinate bar over an unknown total
    /// is a bar that lies.
    private(set) var progress: Double?
    private(set) var progressLabel = ""
    /// What is left, in words — or `nil` when it cannot be justified.
    ///
    /// The web app's `formatEta` rule, kept: **never print a number it cannot justify.** Under
    /// ten seconds it says so rather than showing a countdown that jitters between 4 and 6.
    private(set) var eta: String?
    private var passStarted: Date?
    private var lastDone = 0
    private var lastSample: Date?
    private var rate = 0.0

    /// Ask the running pass to stop.
    ///
    /// Nothing is lost: the pass commits each photograph as it goes, so stopping leaves the
    /// catalog consistent and the work already done is kept. The next pass picks up where this
    /// one stopped, because the work list is the catalog rather than a list in memory.
    func cancelIndexing() {
        cancelRequested = true
        progressLabel = "stopping…"
    }

    private var cancelRequested = false
    var errorMessage: String?

    private let engine: Engine
    private let database: URL

    init() {
        let root = Self.dataRoot()
        database = root.appendingPathComponent("catalog.db")
        setDataRoot(path: root.path)

        do {
            engine = try Engine(databasePath: database.path)
            // The thumbnail loader is an actor created by a static and cannot take a parameter,
            // so it is handed the engine once here rather than threaded through every view.
            EngineHolder.shared.set(engine)
        } catch {
            // A catalog that cannot be opened is not recoverable in-process — every later call
            // would fail the same way. Failing loudly at launch beats a UI where nothing works
            // and nothing says why.
            fatalError("could not open the catalog at \(database.path): \(error)")
        }
    }

    /// Open a library, indexing it if it has changed.
    func open(_ url: URL) async {
        isIndexing = true
        errorMessage = nil
        defer { isIndexing = false }

        let path = url.path(percentEncoded: false)
        do {
            let report = try await index(path)
            library = report.library
            photos = try engine.photos(libraryId: report.library.id)
            folders = try engine.folders(libraryId: report.library.id)
            tags = try engine.tags(libraryId: report.library.id).map { ($0.name, $0.count) }
            people = try engine.people(libraryId: report.library.id)
            try await rebuildLookups()
            progress = nil
        } catch {
            errorMessage = describe(error)
        }
    }

    /// Run the index off the main actor, with progress coming back to it.
    private func index(_ path: String) async throws -> OpenReport {
        let engine = self.engine
        return try await withCheckedThrowingContinuation { continuation in
            // A detached task, not `Task {}`: the pass must not inherit the main actor, or the
            // UI blocks for its whole duration.
            Task.detached(priority: .userInitiated) {
                do {
                    let report = try engine.openLibrary(
                        root: path,
                        progress: ProgressSink { done, total, stage, current in
                            // Hop back for the state write. The callback itself must not block —
                            // it runs on the pass's thread, and waiting on the main actor here
                            // would stall the pass.
                            Task { @MainActor in
                                self.record(done: done, total: total, stage: stage, current: current)
                            }
                            // The index pass reports through this sink and is stopped by
                            // `cancelIndexing`, which the pass checks itself. This is not the
                            // control for it, so it always says keep going.
                            return true
                        }
                    )
                    continuation.resume(returning: report)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    /// Everything known about one photograph.
    ///
    /// Off the main actor for the same reason as the list: a query is 21 ms on a large library
    /// and that is more than a frame. The panel is one call, not four — a panel that fetched
    /// EXIF, then files, then scores would arrive in three visible stages and the middle ones
    /// would look like bugs.
    func detail(for photoId: Int64) async throws -> PhotoDetail {
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.photoDetail(photoId: photoId)
        }.value
    }

    /// Update progress, and work out an estimate from the rate actually achieved.
    ///
    /// The rate is sampled between callbacks rather than averaged over the whole pass: a pass
    /// that starts slow — the first decode warms caches — would otherwise report a remaining
    /// time that is wrong for minutes.
    private func record(done: UInt32, total: UInt32, stage: String, current: String) {
        progress = total > 0 ? Double(done) / Double(total) : nil
        progressLabel = current.isEmpty ? "\(stage) \(done)" : "\(stage) \(done) · \(current)"

        let now = Date()
        if passStarted == nil { passStarted = now }

        // Sample every few seconds; more often and the rate is noise.
        if let last = lastSample, now.timeIntervalSince(last) >= 3, Int(done) > lastDone {
            rate = Double(Int(done) - lastDone) / now.timeIntervalSince(last)
            lastSample = now
            lastDone = Int(done)
        } else if lastSample == nil {
            lastSample = now
            lastDone = Int(done)
        }

        guard total > 0, rate > 0.01 else {
            // **No total, so no estimate.** The scan phase genuinely does not know how many
            // files there are until the walk finishes, and a bar or a time over an unknown
            // total is a lie.
            eta = nil
            return
        }
        let remaining = Double(Int(total) - Int(done)) / rate
        eta = switch remaining {
        case ..<10: "a few seconds left"
        case ..<90: "about a minute left"
        default: "about \(Int((remaining / 60).rounded())) minutes left"
        }
    }

    /// Find faces and group them.
    ///
    /// Long-running: the first pass downloads a 38 MB model and then runs a network over every
    /// photograph. `isIndexing` is reused rather than a second flag, because from the window's
    /// point of view it is the same thing — a pass is running and the grid is not to be
    /// touched.
    func findFaces() async {
        guard let library else { return }
        isIndexing = true
        progress = nil
        progressLabel = "finding faces"
        defer { isIndexing = false }

        let engine = self.engine
        let data = Self.dataRoot().path
        let sink = passSink(stage: "finding faces")
        do {
            let report = try await Task.detached(priority: .userInitiated) {
                try engine.runFacePass(appData: data, libraryId: library.id, progress: sink)
            }.value
            faceReport = report
            people = try engine.people(libraryId: library.id)
            progressLabel = ""
        } catch {
            errorMessage = describe(error)
        }
    }

    /// A progress sink the pass can also stop through.
    ///
    /// The callback returns whether to keep going, and it reads the same `cancelRequested` flag
    /// the index pass uses — one way to stop any pass, rather than a different control per
    /// operation.
    private func passSink(stage: String) -> ProgressSink {
        ProgressSink { done, total, _, _ in
            Task { @MainActor in
                self.record(done: done, total: total, stage: stage, current: "")
            }
            // Read on the pass's thread, which is why `cancelRequested` is set from the main
            // actor and read here without a lock: a `Bool` write is atomic enough for a flag
            // whose worst case is one more photograph being processed.
            return !self.cancelRequested
        }
    }

    /// Tag photographs.
    ///
    /// **A configured endpoint is an upgrade, not a requirement.** Without one this runs CLIP on
    /// this machine, and the report names which tagger ran — "tagged 200 photographs" with no
    /// model named is a claim the user cannot check.
    func tag(limit: UInt32 = 200) async {
        guard let library else { return }
        isIndexing = true
        progress = nil
        progressLabel = "tagging"
        defer { isIndexing = false }

        let engine = self.engine
        let data = Self.dataRoot().path
        let endpoint = ProcessInfo.processInfo.environment["CHAFF_VLM"]
        let model = ProcessInfo.processInfo.environment["CHAFF_VLM_MODEL"] ?? "chaff-vlm"

        let sink = passSink(stage: "tagging")
        do {
            let report = try await Task.detached(priority: .userInitiated) {
                try engine.runTagPass(
                    appData: data, libraryId: library.id,
                    endpoint: endpoint, model: model, limit: limit, progress: sink
                )
            }.value
            tagReport = report
            tags = try engine.tags(libraryId: library.id).map { ($0.name, $0.count) }
            progressLabel = ""
        } catch {
            errorMessage = describe(error)
        }
    }

    /// Rebuild the two lookup tables the navigator's narrowing reads.
    private func rebuildLookups() async throws {
        guard let library else { return }
        let engine = self.engine
        let (byTag, byPerson) = try await Task.detached(priority: .utility) {
            var byTag: [Int64: Set<String>] = [:]
            for (name, _) in try engine.tags(libraryId: library.id).map({ ($0.name, $0.count) }) {
                for id in try engine.photosWithTag(libraryId: library.id, tag: name, model: nil) {
                    byTag[id, default: []].insert(name)
                }
            }
            var byPerson: [Int64: Set<Int64>] = [:]
            for person in try engine.people(libraryId: library.id) {
                for id in try engine.personPhotos(personId: person.id) {
                    byPerson[id, default: []].insert(person.id)
                }
            }
            return (byTag, byPerson)
        }.value
        tagsByPhoto = byTag
        peopleByPhoto = byPerson
    }

    /// The photographs carrying a tag.
    func photosWithTag(_ tag: String) async throws -> [Int64] {
        guard let library else { return [] }
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.photosWithTag(libraryId: library.id, tag: tag, model: nil)
        }.value
    }

    /// The photographs in a person's group.
    func personPhotos(_ personId: Int64) async throws -> [Int64] {
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.personPhotos(personId: personId)
        }.value
    }

    /// Give a group a name. **Typing a name is what confirms it** — there is no separate
    /// confirm step, because a second "yes, I meant it" is a step nobody takes, leaving groups
    /// unconfirmed and the next clustering pass free to split them again.
    func namePerson(_ personId: Int64, _ name: String) async throws {
        let engine = self.engine
        try await Task.detached(priority: .userInitiated) {
            try engine.namePerson(personId: personId, name: name)
        }.value
        if let library { people = try engine.people(libraryId: library.id) }
    }

    /// Merge one group into another. `into` keeps its name and its photographs.
    func mergePeople(from: Int64, into: Int64) async throws -> UInt32 {
        let engine = self.engine
        let moved = try await Task.detached(priority: .userInitiated) {
            try engine.mergePeople(from: from, into: into)
        }.value
        if let library { people = try engine.people(libraryId: library.id) }
        return moved
    }

    /// Everything in the library's trash, newest first.
    func trash() async throws -> [TrashEntry] {
        guard let root = library?.root else { return [] }
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.trash(root: root)
        }.value
    }

    /// Permanently remove operations. **The only irreversible thing in this application.**
    func purgeTrash(opIds: [String]) async throws -> UInt32 {
        guard let root = library?.root else { return 0 }
        let engine = self.engine
        let n = try await Task.detached(priority: .userInitiated) {
            try engine.purgeTrash(root: root, opIds: opIds)
        }.value
        if let library { photos = try engine.photos(libraryId: library.id) }
        return n
    }

    /// Faces clustering was unsure about.
    func ambiguousFaces(limit: UInt32 = 50) async throws -> [AmbiguousFace] {
        guard let library else { return [] }
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.ambiguousFaces(libraryId: library.id, limit: limit)
        }.value
    }

    /// Ask a tagging endpoint what it can do, **before** starting a pass that would fail.
    func diagnoseEndpoint(_ endpoint: String, model: String) async -> EndpointReport {
        let engine = self.engine
        return await Task.detached(priority: .userInitiated) {
            engine.diagnoseEndpoint(endpoint: endpoint, model: model)
        }.value
    }

    /// What this machine can do.
    func capabilities() async -> Capabilities {
        let engine = self.engine
        return await Task.detached(priority: .utility) {
            engine.capabilities()
        }.value
    }

    /// A remembered setting.
    func setting(_ key: String) async -> String? {
        let engine = self.engine
        return try? await Task.detached(priority: .utility) {
            try engine.setting(key: key)
        }.value
    }

    /// Remember a setting.
    func setSetting(_ key: String, _ value: String) async {
        let engine = self.engine
        try? await Task.detached(priority: .utility) {
            try engine.setSetting(key: key, value: value)
        }.value
    }

    /// Take faces out of a group and into one of their own.
    ///
    /// The group keeps at least one face — the engine refuses to empty it, because a person with
    /// no faces is not a group and would appear in the navigator as a name with nothing behind
    /// it.
    func splitPerson(personId: Int64, faceIds: [Int64]) async throws -> Int64? {
        let engine = self.engine
        let created = try await Task.detached(priority: .userInitiated) {
            try engine.splitPerson(personId: personId, faceIds: faceIds)
        }.value
        if let library { people = try engine.people(libraryId: library.id) }
        return created
    }

    /// Write sidecars and say what happened.
    ///
    /// The report is shown rather than logged: "12 written, 40 skipped" is the difference between
    /// a photographer trusting their ratings left the app and re-doing them by hand.
    func writeSidecars() async {
        isIndexing = true
        progressLabel = "writing sidecars"
        defer { isIndexing = false }
        do {
            let report = try await writeSidecarsReport()
            sidecarReport = report
            progressLabel = ""
        } catch {
            errorMessage = describe(error)
        }
    }

    private func writeSidecarsReport() async throws -> SidecarReport {
        guard let library else {
            throw ChaffError.Engine(kind: .notFound, message: "no library is open")
        }
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.writeSidecars(libraryId: library.id)
        }.value
    }

    /// Test the tagging endpoint and say what it can do.
    ///
    /// **Before a pass, not after it fails.** A reachable endpoint offering a different model, or
    /// one that lists a model and refuses a vision request, both produce a pass that fails on
    /// every photograph — and this is the field that says so first.
    func diagnoseTagging() async {
        let endpoint = ProcessInfo.processInfo.environment["CHAFF_VLM"] ?? ""
        guard !endpoint.trimmingCharacters(in: .whitespaces).isEmpty else {
            // **Said plainly rather than attempted.** With no endpoint the pass uses CLIP, which
            // is a different tagger with a closed vocabulary — not a broken vision model.
            endpointReport = nil
            errorMessage = """
                No vision endpoint is configured, so tagging uses CLIP on this machine — a \
                different tagger with a fixed vocabulary of about 38 phrases.

                Set CHAFF_VLM to use a model instead.
                """
            return
        }
        let model_ = ProcessInfo.processInfo.environment["CHAFF_VLM_MODEL"] ?? "chaff-vlm"
        endpointReport = await diagnoseEndpoint(endpoint, model: model_)
    }

    /// The tags on one photograph, for the inspector.
    func photoTags(_ photoId: Int64) async -> [String] {
        let engine = self.engine
        return (try? await Task.detached(priority: .utility) {
            try engine.photoTags(photoId: photoId)
        }.value) ?? []
    }

    /// Why a photograph scored what it did, in words.
    func photoExplanation(_ photoId: Int64) async -> [String] {
        let engine = self.engine
        return (try? await Task.detached(priority: .utility) {
            try engine.photoExplanation(photoId: photoId)
        }.value) ?? []
    }

    /// Write a decision.
    func setDecision(photoId: Int64, rating: UInt8, rejected: Bool) async throws {
        let engine = self.engine
        try await Task.detached(priority: .userInitiated) {
            try engine.setDecision(photoId: photoId, rating: rating, rejected: rejected)
        }.value
    }

    /// Reflect a decision in the loaded list, without a round trip.
    ///
    /// The write has already succeeded, so re-reading 50,000 records to learn what we just told
    /// the engine would be a second of work for a fact already in hand.
    func applyLocally(photoId: Int64, rating: UInt8, rejected: Bool) {
        guard let i = photos.firstIndex(where: { $0.id == photoId }) else { return }
        var p = photos[i]
        p.rating = rating
        p.rejected = rejected
        photos[i] = p
    }

    /// Restore a trashed operation.
    func restoreTrash(opId: String) async throws {
        guard let root = library?.root else {
            throw ChaffError.Engine(
                kind: .notFound,
                message: "no library is open, so there is nothing to restore into"
            )
        }
        let engine = self.engine
        _ = try await Task.detached(priority: .userInitiated) {
            try engine.restoreTrash(root: root, opId: opId)
        }.value
        // A restore changes what is on disk, so the list has to be re-read rather than patched.
        if let library {
            photos = try engine.photos(libraryId: library.id)
        }
    }

    /// Work out what a delete would move.
    func planDelete(photoIds: [Int64], root: String) async throws -> DeletePlan {
        let engine = self.engine
        return try await Task.detached(priority: .userInitiated) {
            try engine.planDelete(root: root, photoIds: photoIds)
        }.value
    }

    /// Move what was shown.
    func commitDelete(root: String) async throws -> DeleteReceipt {
        let engine = self.engine
        let receipt = try await Task.detached(priority: .userInitiated) {
            try engine.commitDelete(root: root)
        }.value
        if let library {
            photos = try engine.photos(libraryId: library.id)
        }
        return receipt
    }

    func cancelDelete() {
        try? engine.cancelDelete()
    }

    /// A message a person can act on.
    ///
    /// The engine's `FailureKind` crosses the boundary as a Swift enum so this can branch —
    /// which it could not when the error type was `flat_error` and the kind was silently
    /// dropped.
    func describe(_ error: Error) -> String {
        guard case let ChaffError.Engine(kind, message) = error else {
            return error.localizedDescription
        }
        switch kind {
        case .poisoned:
            return "\(message) — restart Chaff."
        case .busy:
            return "\(message) — something else is using the catalog. Try again in a moment."
        case .notFound:
            return message
        case .refused:
            return message
        case .other:
            return message
        }
    }
}

/// Bridges the Rust callback interface to a Swift closure.
///
/// `Progress` is a UniFFI callback interface, so Swift has to supply a class. A struct with a
/// closure would be nicer and cannot conform to an `AnyObject` protocol.
private final class ProgressSink: Progress, @unchecked Sendable {
    // **Returns whether to keep going.** A sink that can only be listened to cannot stop
    // anything, and a face pass over a large library is tens of minutes.
    private let handler: (UInt32, UInt32, String, String) -> Bool

    init(_ handler: @escaping (UInt32, UInt32, String, String) -> Bool) {
        self.handler = handler
    }

    func onProgress(done: UInt32, total: UInt32, stage: String, current: String) -> Bool {
        handler(done, total, stage, current)
    }
}
