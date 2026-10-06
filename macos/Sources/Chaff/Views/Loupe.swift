import SwiftUI
import chaff_ffiFFI

/// One photograph, as large as the window allows.
///
/// # Why this is not optional in a culling tool
///
/// A 256-pixel tile is enough to see what a photograph *is* and not enough to decide whether to
/// keep it. Focus, expression and the moment are all judgements that need the frame at size —
/// and a culling tool that only shows tiles is one people open a second application beside.
///
/// # What it does not do
///
/// It does not zoom to 100%. The engine has a 2048-pixel `Zoom` size and it is the right call for
/// judging focus, but it is a second interaction (a click, a pan, a way back) and half a feature
/// done is worse than one deferred. Filed rather than half-built.
struct Loupe: View {
    @Environment(EngineModel.self) private var model
    @Environment(Culling.self) private var culling

    /// The photographs being paged through — the grid's visible list, passed in so the loupe
    /// navigates **what the user was looking at** rather than the whole library.
    let photos: [Photo]
    @Binding var isPresented: Bool

    @State private var image: NSImage?
    @State private var loading = true

    /// Where the cursor is within `photos`.
    private var index: Int? {
        guard let id = culling.cursor else { return nil }
        return photos.firstIndex { $0.id == id }
    }

    private var photo: Photo? {
        guard let index else { return nil }
        return photos[index]
    }

    var body: some View {
        ZStack {
            // **Black, not a material.** Everything in this application is glass or the window's
            // own background; a photograph being judged needs neither behind it, because any
            // tint is a colour cast on the thing being judged.
            Color.black.ignoresSafeArea()

            if let photo {
                if let image {
                    Image(nsImage: image)
                        .resizable()
                        .aspectRatio(contentMode: .fit)
                        .padding(40)
                } else if loading {
                    ProgressView().controlSize(.large).tint(.white)
                } else {
                    // **Said, not blank.** A photograph that could not be rendered reads as
                    // "broken" rather than as "still loading", which is the difference between a
                    // user moving on and a user waiting.
                    ContentUnavailableView(
                        "Could not open this photograph",
                        systemImage: "exclamationmark.triangle",
                        description: Text(
                            "The file may have moved, or it is a format this build cannot decode."
                        )
                    )
                }

                VStack {
                    Spacer()
                    HStack(spacing: 14) {
                        Text(photo.stem).font(.callout).lineLimit(1)
                        if let composite = photo.composite {
                            Text("\(Int(composite))").font(.callout).monospacedDigit()
                        }
                        if photo.rejected {
                            Text("rejected").font(.caption).foregroundStyle(.red)
                        }
                        if photo.rating > 0 {
                            Text(String(repeating: "★", count: Int(photo.rating)))
                                .font(.caption)
                                .foregroundStyle(.yellow)
                        }
                        Spacer()
                        Text("\((index ?? 0) + 1) of \(photos.count)")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .foregroundStyle(.white)
                    .padding(.horizontal, 24)
                    .padding(.vertical, 12)
                }
            } else {
                // No cursor means nothing to show — not an error, and not a spinner.
                ContentUnavailableView(
                    "Nothing selected",
                    systemImage: "photo",
                    description: Text("Choose a photograph, then press Space.")
                )
            }
        }
        // The keys that make a loupe usable. **The same ones the grid uses**, so paging through a
        // shoot does not need a different vocabulary in each view.
        .onKeyPress(.leftArrow) { step(-1); return .handled }
        .onKeyPress(.rightArrow) { step(1); return .handled }
        .onKeyPress(.escape) { isPresented = false; return .handled }
        .onKeyPress(.space) { isPresented = false; return .handled }
        .onKeyPress(keys: ["1", "2", "3", "4", "5"]) { press in
            guard let n = UInt8(press.characters), let photo else { return .ignored }
            Task { await rate(n, photo: photo) }
            return .handled
        }
        .onKeyPress(keys: ["x", "X"]) { _ in
            guard let photo else { return .ignored }
            Task { await reject(photo: photo) }
            return .handled
        }
        .task(id: culling.cursor) {
            guard let id = culling.cursor else {
                image = nil
                return
            }
            loading = true
            image = await ThumbnailLoader.shared.loupe(for: id)
            loading = false
        }
    }

    /// Move the cursor, which is what the grid and the loupe both follow.
    private func step(_ delta: Int) {
        guard let index else { return }
        let next = index + delta
        guard photos.indices.contains(next) else { return }
        culling.cursor = photos[next].id
    }

    private func rate(_ rating: UInt8, photo: Photo) async {
        do {
            try await model.setDecision(photoId: photo.id, rating: rating, rejected: photo.rejected)
            model.applyLocally(photoId: photo.id, rating: rating, rejected: photo.rejected)
        } catch {
            model.errorMessage = model.describe(error)
        }
    }

    private func reject(photo: Photo) async {
        do {
            try await model.setDecision(photoId: photo.id, rating: photo.rating, rejected: !photo.rejected)
            model.applyLocally(photoId: photo.id, rating: photo.rating, rejected: !photo.rejected)
        } catch {
            model.errorMessage = model.describe(error)
        }
    }
}
