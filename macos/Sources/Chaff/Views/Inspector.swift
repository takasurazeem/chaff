import SwiftUI
import chaff_ffiFFI

/// What is known about the selected photograph.
///
/// # Why a `Form` and not a grid of `Text`
///
/// `LabeledContent` inside a `Form` gets keyboard navigation, VoiceOver labels, correct
/// alignment and the platform's own row height for free. A hand-rolled panel has to reimplement
/// all four and usually does three of them badly — which is the argument for a native shell in
/// the first place.
///
/// # Two decisions worth naming
///
/// **Shutter speed reads `1/500`, not `0.002`.** Every camera and every editor writes it the
/// first way; a panel that does not makes people convert in their head.
///
/// **Capture time is labelled "camera clock".** The engine treats the camera's local wall-clock
/// as UTC, so differences between photographs are right and the absolute moment is not.
/// Printing a bare time would claim a precision the data does not have.
struct Inspector: View {
    @Environment(EngineModel.self) private var model
    let photoId: Int64?

    @State private var detail: PhotoDetail?
    @State private var loadFailed = false

    /// What the panel is doing, as a value.
    ///
    /// **Three states, not two.** The first version had `detail`, `loadFailed`, and "everything
    /// else" — and "everything else" was both *loading* and *nothing selected*. Those are
    /// different: one is work in progress and the other is an empty panel, and showing a spinner
    /// for the second says "working on it" when there is nothing to work on. A user saw a
    /// spinner in an empty inspector and reasonably read it as stuck.
    private enum State {
        case nothingSelected
        case loading
        case failed
        case loaded(PhotoDetail)
    }

    private var state: State {
        if photoId == nil { return .nothingSelected }
        if loadFailed { return .failed }
        if let detail { return .loaded(detail) }
        return .loading
    }

    var body: some View {
        Group {
            switch state {
            case let .loaded(detail):
                content(detail)
            case .nothingSelected:
                ContentUnavailableView(
                    "Nothing selected",
                    systemImage: "photo",
                    description: Text("Choose a photograph to see its camera, exposure and score.")
                )
            case .failed:
                ContentUnavailableView(
                    "Could not read that photograph",
                    systemImage: "exclamationmark.triangle",
                    description: Text("The catalog has a record of it but the details did not load.")
                )
            case .loading:
                ProgressView().controlSize(.small)
            }
        }
        .task(id: photoId) {
            // `.task(id:)` cancels the previous load when the selection moves, so arrowing
            // through a shoot does not leave a stale panel behind a faster one.
            guard let photoId else {
                detail = nil
                return
            }
            detail = nil
            loadFailed = false
            do {
                detail = try await model.detail(for: photoId)
            } catch {
                loadFailed = true
            }
        }
    }

    @ViewBuilder
    private func content(_ d: PhotoDetail) -> some View {
        Form {
            Section {
                VStack(alignment: .leading, spacing: 2) {
                    Text(d.stem).font(.headline).lineLimit(1).truncationMode(.middle)
                    Text(URL(fileURLWithPath: d.dir).lastPathComponent)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .help(d.dir)
                }
            }

            if d.composite != nil || !d.terms.isEmpty {
                Section("Score") {
                    if let composite = d.composite {
                        LabeledContent("Overall") {
                            HStack(spacing: 6) {
                                Text("\(Int(composite))").monospacedDigit()
                                if let band = d.band {
                                    Text(band.capitalized)
                                        .font(.caption)
                                        .padding(.horizontal, 6)
                                        .padding(.vertical, 1)
                                        .background(bandColor(band).opacity(0.2), in: Capsule())
                                        .foregroundStyle(bandColor(band))
                                }
                            }
                        }
                    }

                    // **The answer to "why 62?".** A composite alone is a number nobody can act
                    // on; the per-term percentiles are what make it checkable, and the label
                    // says what they are relative to — "80th" means nothing without "of what?".
                    ForEach(d.terms, id: \.label) { term in
                        LabeledContent(term.label) {
                            HStack(spacing: 6) {
                                ProgressView(value: term.percentile, total: 100)
                                    .frame(width: 70)
                                Text("\(Int(term.percentile))")
                                    .monospacedDigit()
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }

            Section("Camera") {
                row("Camera", d.camera)
                row("Lens", d.lens)
                if let iso = d.iso { LabeledContent("ISO", value: "\(iso)") }
                row("Aperture", formatAperture(d.fNumber))
                row("Shutter", formatShutter(d.exposureTime))
                row("Focal length", formatFocal(d.focalLength))
                row("Captured", formatCaptured(d.capturedAt))
            }

            Section("Files") {
                ForEach(d.files, id: \.path) { file in
                    HStack(spacing: 6) {
                        Text(file.role.uppercased())
                            .font(.caption2)
                            .foregroundStyle(.tertiary)
                            .frame(width: 46, alignment: .leading)
                        Text(file.name)
                            .font(.caption)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .help(file.path)
                        Spacer(minLength: 4)
                        Text(ByteCountFormatStyle().format(Int64(file.sizeBytes)))
                            .font(.caption)
                            .monospacedDigit()
                            .foregroundStyle(.secondary)
                    }
                }
            }

            if d.needsReview {
                Section {
                    Label(
                        "Chaff is unsure about this photograph and would like you to look",
                        systemImage: "questionmark.circle"
                    )
                    .font(.caption)
                    .foregroundStyle(.orange)
                }
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 240)
    }

    @ViewBuilder
    private func row(_ label: String, _ value: String?) -> some View {
        // **Absent fields are omitted, not shown empty.** A JPEG with its metadata stripped is
        // a normal thing, and a panel that renders an empty row for it looks broken.
        if let value, !value.isEmpty {
            LabeledContent(label, value: value)
        }
    }

    private func bandColor(_ band: String) -> Color {
        switch band {
        case "keep": .green
        case "reject": .red
        default: .orange
        }
    }
}

// MARK: - Formatting
//
// Free functions rather than view methods so they are testable without a view — the web app
// learned that lesson when the same helpers had to move out of a component to satisfy a lint
// rule about fast refresh.

/// `1/500` rather than `0.002`. Every camera and every editor writes it the first way.
func formatShutter(_ seconds: Double?) -> String? {
    guard let seconds, seconds > 0 else { return nil }
    if seconds >= 1 { return String(format: "%.1f s", seconds) }
    return "1/\(Int((1 / seconds).rounded()))"
}

func formatAperture(_ f: Double?) -> String? {
    guard let f, f > 0 else { return nil }
    return f == f.rounded() ? "f/\(Int(f))" : String(format: "f/%.1f", f)
}

/// Rounded, because nobody shoots 24.7 mm.
func formatFocal(_ mm: Double?) -> String? {
    guard let mm, mm > 0 else { return nil }
    return "\(Int(mm.rounded())) mm"
}

/// Labelled as the camera's clock, because that is what it is.
func formatCaptured(_ epoch: Int64?) -> String? {
    guard let epoch else { return nil }
    let date = Date(timeIntervalSince1970: TimeInterval(epoch))
    let formatter = DateFormatter()
    formatter.dateFormat = "yyyy-MM-dd HH:mm"
    formatter.timeZone = TimeZone(secondsFromGMT: 0)
    return "\(formatter.string(from: date)) (camera clock)"
}
