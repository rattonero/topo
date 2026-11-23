import SwiftUI

/// An illustration on its own, to pinch, drag and look at closely.
struct ImageZoomView: View {
    let image: PlatformImage
    let close: () -> Void

    @State private var scale = 1.0
    @State private var settledScale = 1.0
    @State private var offset = CGSize.zero
    @State private var settledOffset = CGSize.zero

    private let range = 1.0...8.0

    var body: some View {
        ZStack {
            Color.black.opacity(0.92).ignoresSafeArea()
                .onTapGesture(perform: close)

            Image(platformImage: image)
                .resizable()
                .scaledToFit()
                .scaleEffect(scale)
                .offset(offset)
                .gesture(drag.simultaneously(with: magnify))
                .onTapGesture(count: 2) { zoom(to: scale > 1.01 ? 1 : 2.5) }
                .padding(12)
                .accessibilityLabel("Illustration")
        }
        .overlay(alignment: .topTrailing) {
            Button("Close", systemImage: "xmark", action: close)
                .labelStyle(.iconOnly)
                .keyboardShortcut(.cancelAction)
                .padding(16)
        }
        .overlay(alignment: .bottom) {
            HStack(spacing: 16) {
                Button("Zoom Out", systemImage: "minus.magnifyingglass") { zoom(to: scale / 1.5) }
                    .disabled(scale <= range.lowerBound)
                Button("Zoom In", systemImage: "plus.magnifyingglass") { zoom(to: scale * 1.5) }
                    .disabled(scale >= range.upperBound)
            }
            .labelStyle(.iconOnly)
            .padding(.bottom, 20)
        }
        .buttonStyle(.bordered)
        .controlSize(.large)
        .environment(\.colorScheme, .dark)
    }

    private var magnify: some Gesture {
        MagnifyGesture()
            .onChanged { scale = clamp(settledScale * $0.magnification) }
            .onEnded { _ in
                settledScale = scale
                if scale <= 1.01 { zoom(to: 1) }
            }
    }

    private var drag: some Gesture {
        DragGesture()
            .onChanged {
                guard scale > 1.01 else { return }
                offset = CGSize(width: settledOffset.width + $0.translation.width, height: settledOffset.height + $0.translation.height)
            }
            .onEnded { _ in settledOffset = offset }
    }

    private func clamp(_ value: Double) -> Double {
        min(max(value, range.lowerBound), range.upperBound)
    }

    private func zoom(to value: Double) {
        withAnimation(.snappy(duration: 0.25)) {
            scale = clamp(value)
            settledScale = scale
            if scale <= 1.01 {
                offset = .zero
                settledOffset = .zero
            }
        }
    }
}
