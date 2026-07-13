import SwiftUI

/// One numeric setting laid out as a single row:
///
///     Label                                  [Number ⊕]  Default: x.xxxx  ⤺
///
/// Replaces the older slider style with a typed number input plus a
/// reset button. The recommended default is shown alongside so users
/// know what the safe value is.
struct NumericSettingRow<T: Numeric & LosslessStringConvertible>: View where T: Comparable {
    let title: String
    let help: String
    let unit: String
    let formatter: NumberFormatter
    @Binding var value: T
    let defaultValue: T
    let range: ClosedRange<T>?

    init(
        title: String,
        help: String = "",
        unit: String = "",
        formatter: NumberFormatter,
        value: Binding<T>,
        defaultValue: T,
        range: ClosedRange<T>? = nil
    ) {
        self.title = title
        self.help = help
        self.unit = unit
        self.formatter = formatter
        self._value = value
        self.defaultValue = defaultValue
        self.range = range
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.body)
                if !help.isEmpty {
                    Text(help)
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
            }
            Spacer(minLength: 8)
            HStack(spacing: 4) {
                TextField("", value: $value, formatter: formatter)
                    .textFieldStyle(.roundedBorder)
                    .multilineTextAlignment(.trailing)
                    .frame(width: 80)
                    .onChange(of: value) { _, new in
                        if let range, !range.contains(new) {
                            value = min(max(new, range.lowerBound), range.upperBound)
                        }
                    }
                if !unit.isEmpty {
                    Text(unit)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .frame(width: 22, alignment: .leading)
                }
            }
            Text("Default: \(formatter.string(for: defaultValue) ?? "")\(unit.isEmpty ? "" : " \(unit)")")
                .font(.caption2)
                .foregroundColor(.secondary)
                .frame(width: 100, alignment: .trailing)
            Button {
                value = defaultValue
            } label: {
                Image(systemName: "arrow.counterclockwise")
            }
            .buttonStyle(.borderless)
            .help("Reset to recommended default")
        }
    }
}
