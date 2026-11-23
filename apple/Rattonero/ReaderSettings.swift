import SwiftUI

enum ReaderTheme: String, CaseIterable, Identifiable {
    case system, light, sepia, dark

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: "Auto"
        case .light: "Light"
        case .sepia: "Sepia"
        case .dark: "Dark"
        }
    }

    /// The scheme the window chrome should take while reading.
    var colorScheme: ColorScheme? {
        switch self {
        case .system: nil
        case .light, .sepia: .light
        case .dark: .dark
        }
    }

    /// Name understood by the core's reader stylesheet.
    func cssName(system: ColorScheme) -> String {
        switch self {
        case .system: system == .dark ? "dark" : "light"
        default: rawValue
        }
    }

    /// Page colour; keep in sync with `core/src/reader.css`.
    func background(system: ColorScheme) -> Color {
        switch cssName(system: system) {
        case "dark": Color(red: 0x1c / 255, green: 0x1c / 255, blue: 0x1e / 255)
        case "sepia": Color(red: 0xf4 / 255, green: 0xec / 255, blue: 0xd8 / 255)
        default: .white
        }
    }
}

/// The reader's few controls, remembered between launches.
@Observable
final class ReaderSettings {
    static let fontSizes: ClosedRange<Double> = 12...34
    static let lineHeights: ClosedRange<Double> = 1.1...2.2
    static let margins: ClosedRange<Double> = 8...96

    var theme: ReaderTheme { didSet { defaults.set(theme.rawValue, forKey: "reader.theme") } }
    var fontSize: Double { didSet { defaults.set(fontSize, forKey: "reader.fontSize") } }
    var lineHeight: Double { didSet { defaults.set(lineHeight, forKey: "reader.lineHeight") } }
    var margin: Double { didSet { defaults.set(margin, forKey: "reader.margin") } }

    @ObservationIgnored private let defaults = UserDefaults.standard

    init() {
        func stored(_ key: String, _ fallback: Double, _ range: ClosedRange<Double>) -> Double {
            let value = UserDefaults.standard.double(forKey: key)
            return range.contains(value) ? value : fallback
        }
        theme = ReaderTheme(rawValue: UserDefaults.standard.string(forKey: "reader.theme") ?? "") ?? .system
        fontSize = stored("reader.fontSize", 18, Self.fontSizes)
        lineHeight = stored("reader.lineHeight", 1.5, Self.lineHeights)
        margin = stored("reader.margin", 28, Self.margins)
    }

    /// Settings in the form the pagination script expects.
    func json(system: ColorScheme) -> String {
        let name = theme.cssName(system: system)
        return "{\"theme\":\"\(name)\",\"fontSize\":\(Int(fontSize)),\"lineHeight\":\(String(format: "%.2f", lineHeight)),\"margin\":\(Int(margin))}"
    }
}

/// Theme, text size, margins and line spacing.
struct ReaderSettingsView: View {
    @Environment(ReaderSettings.self) private var settings

    var body: some View {
        @Bindable var settings = settings
        VStack(alignment: .leading, spacing: 16) {
            Picker("Theme", selection: $settings.theme) {
                ForEach(ReaderTheme.allCases) { Text($0.label).tag($0) }
            }
            .pickerStyle(.segmented)
            .labelsHidden()

            row("Text size", "\(Int(settings.fontSize)) pt") {
                Slider(value: $settings.fontSize, in: ReaderSettings.fontSizes, step: 1) {
                    Text("Text size")
                } minimumValueLabel: {
                    Image(systemName: "textformat.size.smaller")
                } maximumValueLabel: {
                    Image(systemName: "textformat.size.larger")
                }
            }
            row("Margins", "\(Int(settings.margin))") {
                Slider(value: $settings.margin, in: ReaderSettings.margins, step: 4) {
                    Text("Margins")
                } minimumValueLabel: {
                    Image(systemName: "arrow.left.and.right.square")
                } maximumValueLabel: {
                    Image(systemName: "rectangle.portrait.inset.filled")
                }
            }
            row("Line spacing", String(format: "%.1f", settings.lineHeight)) {
                Slider(value: $settings.lineHeight, in: ReaderSettings.lineHeights, step: 0.1) {
                    Text("Line spacing")
                } minimumValueLabel: {
                    Image(systemName: "text.justify")
                } maximumValueLabel: {
                    Image(systemName: "line.3.horizontal")
                }
            }
        }
        .labelsHidden()
        .padding(18)
        .frame(width: 300)
    }

    private func row(_ title: String, _ value: String, @ViewBuilder control: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(title)
                Spacer()
                Text(value).monospacedDigit().foregroundStyle(.secondary)
            }
            .font(.subheadline)
            control()
        }
    }
}
