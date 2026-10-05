import SwiftUI
import chaff_ffiFFI

/// What a delete will move, before it moves it.
///
/// # The dialog is the guarantee, not a courtesy
///
/// The engine hashes every file **when this sheet appears** and verifies those hashes when the
/// user confirms. A file whose contents changed in between — a sync client, an editor, a raw
/// still being copied — aborts the whole operation rather than being moved unexamined.
///
/// That promise is only worth anything if the sheet says what will move, which is why the count
/// and the byte total are here rather than a bare "Are you sure?".
///
/// # And it is not the file list
///
/// `commitDelete` takes **no file list**. It commits the plan the user was shown, held in the
/// engine. This view cannot name a file, cannot supply a hash, and cannot widen the operation —
/// which is the invariant the engine's `DeleteSession` exists to keep, now shared by both
/// shells.
// No `@retroactive`: the generated bindings are compiled into **this** module, not a separate
// one, so this is an ordinary extension. The attribute is for conforming a type you do not own,
// and Swift says so rather than silently accepting it.
extension DeletePlan: Identifiable {
    /// The operation's own id, which names the *operation* rather than the view — so a sheet
    /// cannot be shown for one plan and commit another.
    public var id: String { opId }
}

struct DeleteConfirmation: View {
    @Environment(EngineModel.self) private var model
    @Environment(Culling.self) private var culling

    let plan: DeletePlan
    let root: String
    let onFinished: () -> Void

    @State private var working = false
    @State private var refusal: String?
    /// Warnings raised **at commit time** — a file that vanished while the dialog was open.
    ///
    /// The sheet already showed `plan.warnings`, which are the ones known *before* the user
    /// confirms. The receipt's are the ones discovered *during* the move, and they were dropped:
    /// the dialog closed and a receipt saying `moved: 1` for a dialog that showed 2 went
    /// unremarked. A warning nobody sees is not a warning.
    @State private var afterWarnings: [String] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Move to trash?", systemImage: "trash")
                .font(.headline)

            if !afterWarnings.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Label("Moved, with something to report", systemImage: "info.circle")
                        .font(.callout)
                    ForEach(afterWarnings, id: \.self) { warning in
                        Text(warning)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            } else if let refusal {
                // A refusal is not an error the user caused. It says what happened and what to
                // do, and nothing has moved.
                Text(refusal)
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            } else if !plan.refusals.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("This selection cannot be moved:").font(.callout)
                    ForEach(plan.refusals, id: \.self) { reason in
                        Label(reason, systemImage: "exclamationmark.triangle")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
            } else {
                Text(summary)
                    .font(.callout)
                    .fixedSize(horizontal: false, vertical: true)

                if !plan.warnings.isEmpty {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(plan.warnings, id: \.self) { warning in
                            Label(warning, systemImage: "info.circle")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }

                Text("The files go to `.cull-trash` inside the library. **Nothing is deleted** — you can put them back from the Trash panel, or with ⌘Z.")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            HStack {
                Spacer()
                // **Cancel becomes Done once the move has happened.** The operation is not
                // reversible by closing a dialog, and a button still labelled Cancel after the
                // files have moved is a lie about what it does.
                if !afterWarnings.isEmpty {
                    Button("Done") { onFinished() }
                        .keyboardShortcut(.defaultAction)
                } else {
                    Button("Cancel") {
                        // Cancelling clears the plan rather than leaving it. A plan that is not
                        // cancelled sits until the next one replaces it, and a stale plan is one
                        // a stray call could commit.
                        model.cancelDelete()
                        onFinished()
                    }
                    .keyboardShortcut(.cancelAction)

                    Button(working ? "Moving…" : "Move \(plan.files) file\(plan.files == 1 ? "" : "s")") {
                        Task { await confirm() }
                    }
                    .keyboardShortcut(.defaultAction)
                    .disabled(working || plan.files == 0 || !plan.refusals.isEmpty)
                }
            }
        }
        .padding(20)
        .frame(width: 420)
    }

    private var summary: String {
        let size = ByteCountFormatStyle().format(Int64(plan.bytes))
        return "\(plan.photographs) photograph\(plan.photographs == 1 ? "" : "s"), "
            + "\(plan.files) file\(plan.files == 1 ? "" : "s"), \(size)."
    }

    private func confirm() async {
        working = true
        defer { working = false }
        do {
            let receipt = try await model.commitDelete(root: root)
            // Recorded **after** the engine committed: the operation id only exists once it has.
            culling.recordTrash(opId: receipt.opId, moved: receipt.moved)

            // **What the move discovered, before the sheet closes.** A file that vanished while
            // the dialog was open is warned about rather than refused — nine of ten is the right
            // outcome — and the sheet stays up to say so.
            if !receipt.warnings.isEmpty {
                afterWarnings = receipt.warnings
                return
            }
            onFinished()
        } catch {
            // A refusal leaves the sheet open with the reason. The plan is already dropped by
            // the engine, so the user selects again — which is the honest outcome, because the
            // library changed underneath them and the plan is no longer a promise about now.
            refusal = model.describe(error)
        }
    }
}
