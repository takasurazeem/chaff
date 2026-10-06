import SwiftUI
import chaff_ffiFFI

// The generated record has no identity and `.sheet(item:)` needs one.
//
// The engine's `faceId`, which names the **face** rather than the row — so a sheet cannot be
// shown for one face and act on another.
extension AmbiguousFace: Identifiable {
    public var id: Int64 { faceId }
}

/// The faces clustering was unsure about, and what to do with each.
///
/// # Why this is the input to a feature that already exists
///
/// Naming and merging let a user fix a group **after** the fact. This is how they are asked
/// *before* it — and without it the only way to correct a group is to name it wrong first.
///
/// # Why confidence is shown as a number and not a bar
///
/// A bar invites reading it as "how good is this photograph". It is not: it is **how far apart
/// the two nearest groups were**, so a low number means clustering was torn, which is exactly the
/// case worth a person's attention. The label says so.
struct FaceReview: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var queue: [AmbiguousFace] = []
    @State private var loading = true
    @State private var busy = false
    @State private var mergeInto: AmbiguousFace?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Faces to check").font(.headline)
                Spacer()
                if !queue.isEmpty {
                    Text("\(queue.count)").font(.caption).foregroundStyle(.secondary)
                }
                Button("Done") { dismiss() }.keyboardShortcut(.cancelAction)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)

            Divider()

            if loading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if queue.isEmpty {
                ContentUnavailableView(
                    "Nothing to check",
                    systemImage: "checkmark.circle",
                    description: Text(
                        "Every face clustering found belongs clearly to one group. Run Find People again after adding photographs."
                    )
                )
            } else {
                List(queue, id: \.faceId) { face in
                    HStack(spacing: 10) {
                        // **The photograph, not a crop of the face.**
                        //
                        // A face crop needs an FFI call that does not exist — the engine stores
                        // face boxes but nothing exposes one as an image. The photograph is the
                        // next best thing and it is already cached, so the decision is at least
                        // made by looking at the frame the face is in.
                        FaceThumb(photoId: face.photoId)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(face.personName ?? "Not yet grouped")
                                .italic(face.personName == nil)
                            Text(
                                "Clustering was torn between this group and another — \(Int(face.confidence * 100))% apart."
                            )
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }

                        Spacer(minLength: 8)

                        Button("Put in a group…") { mergeInto = face }
                            .disabled(busy)
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .frame(width: 620, height: 460)
        .task { await load() }
        .sheet(item: $mergeInto) { face in
            AssignFaceSheet(face: face, onDone: { Task { await load() } })
        }
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            queue = try await model.ambiguousFaces()
        } catch {
            model.errorMessage = model.describe(error)
        }
    }
}

/// The photograph a face is in, as a small image.
///
/// A separate view so the load is `.task(id:)`-scoped: a row that is recycled for a different
/// face must load **that** face's photograph, and a load started in `body` would show the
/// previous row's.
private struct FaceThumb: View {
    let photoId: Int64
    @State private var image: NSImage?

    var body: some View {
        Group {
            if let image {
                Image(nsImage: image).resizable().scaledToFill()
            } else {
                // **A visible placeholder, not an empty box.** A photograph that could not be
                // read reads as "missing" rather than as "still loading", which is the difference
                // between a user moving on and a user waiting.
                ZStack {
                    Rectangle().fill(.quaternary)
                    Image(systemName: "photo").foregroundStyle(.tertiary)
                }
            }
        }
        .frame(width: 56, height: 56)
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .task(id: photoId) {
            image = await ThumbnailLoader.shared.image(for: photoId)
        }
    }
}

/// Put one face into a group.
private struct AssignFaceSheet: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let face: AmbiguousFace
    let onDone: () -> Void

    @State private var busy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Which group does this face belong to?").font(.headline)
            Text(
                "Adding it to a group **confirms** that group's name, which tells the next clustering pass to leave it alone."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            List(model.people, id: \.id) { person in
                Button {
                    Task { await assign(person.id) }
                } label: {
                    HStack {
                        Text(person.name ?? "Group \(person.id)")
                            .italic(person.name == nil)
                        Spacer(minLength: 8)
                        Text("\(person.photos)").foregroundStyle(.secondary).monospacedDigit()
                    }
                }
                .buttonStyle(.plain)
                .disabled(busy)
            }
            .frame(height: 220)

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
            }
        }
        .padding(20)
        .frame(width: 420)
    }

    private func assign(_ personId: Int64) async {
        busy = true
        defer { busy = false }
        do {
            // Assigning is a merge of one face into a group, which the engine already does for
            // the split case — the same operation seen from the other side.
            _ = try await model.splitPerson(personId: personId, faceIds: [face.faceId])
            onDone()
            dismiss()
        } catch {
            model.errorMessage = model.describe(error)
            dismiss()
        }
    }
}
