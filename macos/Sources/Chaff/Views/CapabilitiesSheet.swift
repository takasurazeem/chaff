import SwiftUI
import chaff_ffiFFI

/// What this machine can do.
///
/// # Why this is worth a menu item
///
/// The engine picks a hardware **tier** and a model to match, and the reasoning is invisible
/// otherwise. A user whose tagging is slow has no way to tell whether that is the model, the
/// machine, or a setting — and "your machine is tier 3 because it has no discrete GPU" is an
/// answer.
///
/// It is also the first thing worth asking for in a bug report.
struct CapabilitiesSheet: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("This machine").font(.headline)

            if let machine = model.machine {
                Form {
                    LabeledContent("Tier", value: machine.tier)
                    if let gpu = machine.gpu {
                        LabeledContent("GPU", value: gpu)
                    }
                    if let vram = machine.vramMb {
                        LabeledContent("VRAM", value: "\(vram) MB")
                    }
                    if machine.unifiedMemory {
                        // **The Apple Silicon case, named.** There is no separate VRAM figure to
                        // read, so a machine is tiered on its total memory — and a user comparing
                        // against a PC with the same RAM would otherwise wonder why the numbers
                        // do not line up.
                        LabeledContent("Memory", value: "unified")
                    }
                }
                .formStyle(.grouped)

                Text(machine.summary)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            } else {
                ProgressView().controlSize(.small)
            }

            if let report = model.endpointReport {
                Divider()
                Text("Tagging endpoint").font(.headline)
                Form {
                    LabeledContent("Reachable", value: report.reachable ? "yes" : "no")
                    LabeledContent("Model present", value: report.modelPresent ? "yes" : "no")
                    LabeledContent("Can tag", value: report.visionOk ? "yes" : "no")
                }
                .formStyle(.grouped)
                Text(report.detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                    .textSelection(.enabled)
            }

            HStack {
                Spacer()
                Button("Done") { dismiss() }.keyboardShortcut(.defaultAction)
            }
        }
        .padding(20)
        .frame(width: 460)
        .task {
            model.machine = await model.capabilities()
        }
    }
}
