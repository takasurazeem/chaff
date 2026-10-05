import SwiftUI
import chaff_ffiFFI

/// What is in the trash, and how to get it back.
///
/// # Why this had to exist before the feature was usable
///
/// The native app could **move files to the trash and not manage them.** ⌘Z reversed the last
/// operation, and after a restart there was nothing: no way to see what had been moved, no way to
/// put it back, no way to reclaim the disk. An application that fills a container it cannot empty
/// is one people stop trusting with their photographs.
///
/// # Operations, not files
///
/// The manifest records what **one confirmation** moved, and restoring is per-operation. A list
/// of individual files would make "put back what I deleted" a matter of selecting the right
/// twelve — which is not a thing anyone should have to do.
struct TrashPanel: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    @State private var entries: [TrashEntry] = []
    @State private var loading = true
    @State private var purging: TrashEntry?
    @State private var busy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack {
                Text("Trash").font(.headline)
                Spacer()
                if totalBytes > 0 {
                    Text(ByteCountFormatStyle().format(Int64(totalBytes)))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)

            Divider()

            if loading {
                ProgressView().controlSize(.small).frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if entries.isEmpty {
                ContentUnavailableView(
                    "Nothing in the trash",
                    systemImage: "trash",
                    description: Text("Photographs you move are kept here until you remove them.")
                )
            } else {
                List(entries, id: \.opId) { entry in
                    HStack(spacing: 10) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(entry.reason).lineLimit(1)
                            HStack(spacing: 6) {
                                Text(date(entry.at))
                                Text("·")
                                Text("\(entry.files) file\(entry.files == 1 ? "" : "s")")
                                Text("·")
                                Text(ByteCountFormatStyle().format(Int64(entry.bytes)))
                                if entry.incomplete {
                                    // **Said rather than hidden.** A restore that brings back
                                    // nine of twelve should not surprise anyone, and the reason
                                    // is usually something outside this app.
                                    Text("·")
                                    Text("some files are gone")
                                        .foregroundStyle(.orange)
                                }
                            }
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 8)
                        Button("Put back") { Task { await restore(entry) } }
                            .disabled(busy)
                        Button("Delete") { purging = entry }
                            .disabled(busy)
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .frame(width: 560, height: 420)
        .task { await load() }
        .alert(
            "Delete permanently?",
            isPresented: Binding(get: { purging != nil }, set: { if !$0 { purging = nil } }),
            presenting: purging
        ) { entry in
            Button("Cancel", role: .cancel) { purging = nil }
            Button("Delete \(entry.files) file\(entry.files == 1 ? "" : "s")", role: .destructive) {
                Task { await purge(entry) }
            }
        } message: { entry in
            // **The only irreversible thing in this application**, and the message says so in
            // the terms that matter: the size is what the user gets back, and there is no undo.
            Text(
                "\(ByteCountFormatStyle().format(Int64(entry.bytes))) will be removed from the trash. This cannot be undone."
            )
        }
    }

    private var totalBytes: UInt64 {
        entries.reduce(0) { $0 + $1.bytes }
    }

    private func date(_ epoch: Int64) -> String {
        let f = DateFormatter()
        f.dateStyle = .medium
        f.timeStyle = .short
        return f.string(from: Date(timeIntervalSince1970: TimeInterval(epoch)))
    }

    private func load() async {
        loading = true
        defer { loading = false }
        do {
            entries = try await model.trash()
        } catch {
            model.errorMessage = model.describe(error)
        }
    }

    private func restore(_ entry: TrashEntry) async {
        busy = true
        defer { busy = false }
        do {
            try await model.restoreTrash(opId: entry.opId)
            await load()
        } catch {
            model.errorMessage = model.describe(error)
        }
    }

    private func purge(_ entry: TrashEntry) async {
        busy = true
        defer { busy = false }
        purging = nil
        do {
            _ = try await model.purgeTrash(opIds: [entry.opId])
            await load()
        } catch {
            model.errorMessage = model.describe(error)
        }
    }
}
