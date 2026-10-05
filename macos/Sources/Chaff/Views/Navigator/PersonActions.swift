import SwiftUI
import chaff_ffiFFI

// The generated `Person` is a plain record with no identity, and `.sheet(item:)` needs one.
//
// No `@retroactive`: the bindings compile into **this** module, not a separate one, so this is an
// ordinary extension. The attribute is for conforming a type you do not own, and Swift says so
// rather than accepting it silently — which is how `DeletePlan` learned the same thing.
//
// `id` is the engine's own, and it is the right one: it names the *group*, so a sheet cannot be
// shown for one and act on another.
extension Person: Identifiable {}

/// Naming a group, and merging one into another.
///
/// # Why naming is a sheet and not an inline field
///
/// A `TextField` in a sidebar row is one mis-click from editing, and the navigator is a list you
/// scroll with the keyboard — so an editable row means arrow keys land in a text field and the
/// list stops moving. A sheet is one deliberate step, and this is a step worth being deliberate
/// about: **typing a name is what confirms a group**, and a confirmed group is one the next
/// clustering pass leaves alone.
///
/// # Why there is no Confirm button
///
/// The web app's reasoning, kept: a name a user has typed *is* the confirmation. A second step
/// to say "yes, I meant it" is a step nobody takes, which leaves groups unconfirmed and free to
/// be split again on the next pass. Naming and confirming are the same act.
struct NamePersonSheet: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    let person: Person
    @State private var name: String = ""
    @State private var busy = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(person.name == nil ? "Name this group" : "Rename this group")
                .font(.headline)

            Text(
                person.name == nil
                    ? "These \(person.photos) photographs were grouped together on this machine. A group is a suggestion until you name it — naming it is what tells the next pass to leave it alone."
                    : "\(person.photos) photographs in this group."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            TextField("Name", text: $name)
                .textFieldStyle(.roundedBorder)
                .onSubmit { Task { await save() } }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(busy ? "Saving…" : "Save") { Task { await save() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy)
            }

            if person.name != nil {
                // **Clearing a name un-confirms the group**, which is a real thing to want: a
                // merge that went wrong, a name typed by mistake. Said out loud rather than left
                // as a surprise when the next pass reshuffles it.
                Text("Clearing the name makes this a suggestion again.")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
            }
        }
        .padding(20)
        .frame(width: 380)
        .onAppear { name = person.name ?? "" }
    }

    private func save() async {
        busy = true
        defer { busy = false }
        do {
            try await model.namePerson(person.id, name)
            dismiss()
        } catch {
            model.errorMessage = model.describe(error)
            dismiss()
        }
    }
}

/// Merge one group into another.
///
/// # Which one survives
///
/// **The one you pick is the survivor** — it keeps its name and gains the other's photographs.
/// The alternative (merge into the one you started from) reads more naturally in a sentence and
/// is wrong in practice: the common correction is "these are all Alice", where Alice already
/// exists and the stray group does not.
struct MergePeopleSheet: View {
    @Environment(EngineModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    /// The group being emptied into another.
    let from: Person
    @State private var into: Int64?
    @State private var busy = false

    /// Every other group, named ones first — merging into an unnamed group throws away the only
    /// name involved, so the list puts the ones worth keeping at the top.
    private var candidates: [Person] {
        model.people
            .filter { $0.id != from.id }
            .sorted {
                if ($0.name == nil) != ($1.name == nil) { return $0.name != nil }
                return $0.photos > $1.photos
            }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Merge “\(from.name ?? "Group \(from.id)")” into…").font(.headline)

            Text(
                "\(from.photos) photograph\(from.photos == 1 ? "" : "s") will join the group you pick. The group you pick keeps its name."
            )
            .font(.caption)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)

            if candidates.isEmpty {
                ContentUnavailableView(
                    "No other group",
                    systemImage: "person.2",
                    description: Text("There is nothing to merge into yet.")
                )
                .frame(height: 120)
            } else {
                List(candidates, selection: $into) { person in
                    HStack {
                        Text(person.name ?? "Group \(person.id)")
                            .italic(person.name == nil)
                            .foregroundStyle(person.name == nil ? .secondary : .primary)
                        Spacer(minLength: 8)
                        Text("\(person.photos)")
                            .foregroundStyle(.secondary)
                            .monospacedDigit()
                    }
                    .tag(person.id)
                }
                .frame(height: 200)
            }

            HStack {
                Spacer()
                Button("Cancel") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(busy ? "Merging…" : "Merge") { Task { await merge() } }
                    .keyboardShortcut(.defaultAction)
                    .disabled(busy || into == nil)
            }
        }
        .padding(20)
        .frame(width: 420)
    }

    private func merge() async {
        guard let into else { return }
        busy = true
        defer { busy = false }
        do {
            _ = try await model.mergePeople(from: from.id, into: into)
            dismiss()
        } catch {
            model.errorMessage = model.describe(error)
            dismiss()
        }
    }
}
