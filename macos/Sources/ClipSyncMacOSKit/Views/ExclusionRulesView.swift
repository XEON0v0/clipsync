import SwiftUI

public struct ExclusionRulesView: View {
    @ObservedObject private var settings: SettingsStore

    public init(settings: SettingsStore) {
        self.settings = settings
    }

    public var body: some View {
        Section("Exclusions") {
            Text("Built-in rules always apply.")
                .font(.footnote)
                .foregroundStyle(.secondary)
            ForEach(BuiltinSensitiveRules.all) { rule in
                Label(rule.description, systemImage: "checkmark.shield")
            }
            ForEach(settings.sensitiveRules) { rule in
                ruleRow(rule)
            }
            .onDelete { offsets in
                settings.sensitiveRules.remove(atOffsets: offsets)
            }
            Button("Add Rule...") {
                settings.sensitiveRules.append(SensitiveRule(pattern: "", isRegex: false))
            }
        }
    }

    private func ruleRow(_ rule: SensitiveRule) -> some View {
        HStack {
            TextField("Keyword or regex", text: binding(for: rule))
                .textFieldStyle(.roundedBorder)
                .foregroundStyle(
                    rule.isRegex && !SensitiveContentFilter.isValidRegex(rule.pattern)
                        ? .red : .primary
                )
            Toggle("Regex", isOn: regexBinding(for: rule))
                .toggleStyle(.checkbox)
        }
    }

    private func binding(for rule: SensitiveRule) -> Binding<String> {
        Binding(
            get: { rule.pattern },
            set: { newValue in
                if let index = settings.sensitiveRules.firstIndex(where: { $0.id == rule.id }) {
                    settings.sensitiveRules[index].pattern = newValue
                }
            }
        )
    }

    private func regexBinding(for rule: SensitiveRule) -> Binding<Bool> {
        Binding(
            get: { rule.isRegex },
            set: { newValue in
                if let index = settings.sensitiveRules.firstIndex(where: { $0.id == rule.id }) {
                    settings.sensitiveRules[index].isRegex = newValue
                }
            }
        )
    }
}
