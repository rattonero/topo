import SwiftUI
import WebKit

/// The book view. EPUBs are paginated by the core; PDFs by PDFKit.
struct ReaderView: View {
    let book: BookInfo

    var body: some View {
        switch book.format {
        case .epub: EpubReaderView(book: book)
        case .pdf: PDFReaderView(book: book)
        }
    }
}

struct EpubReaderView: View {
    let book: BookInfo
    @Environment(LibraryStore.self) private var store
    @Environment(ReaderSettings.self) private var settings
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @State private var model: EpubReaderModel?
    @State private var failure: String?
    @State private var showingSettings = false
    @State private var dictionaryTerm: DictionaryTerm?
    @State private var barInset = 0.0

    var body: some View {
        ZStack {
            settings.theme.background(system: colorScheme).ignoresSafeArea()
            if let model {
                page(model)
            } else if let failure {
                ContentUnavailableView("This book cannot be opened", systemImage: "book.closed", description: Text(failure))
            } else {
                ProgressView()
            }
        }
        .navigationTitle(book.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(model?.controlsVisible == false ? .hidden : .visible, for: .navigationBar)
        .statusBarHidden(model?.controlsVisible == false)
        .animation(.easeInOut(duration: 0.2), value: model?.controlsVisible)
        .sheet(item: $dictionaryTerm) { DictionaryView(term: $0.text).ignoresSafeArea() }
        #endif
        .toolbar { toolbar }
        .preferredColorScheme(settings.theme.colorScheme)
        .task { open() }
        .onChange(of: settings.json(system: colorScheme)) { _, json in
            model?.apply(settingsJSON: json)
        }
    }

    private func open() {
        guard model == nil else { return }
        do {
            let model = try EpubReaderModel(info: book, library: store.library, settingsJSON: settings.json(system: colorScheme))
            model.openExternal = { openURL($0) }
            self.model = model
            Task { await model.start() }
        } catch {
            failure = describe(error)
        }
    }

    private func page(_ model: EpubReaderModel) -> some View {
        stablePage(model) {
            VStack(spacing: 0) {
                WebViewHost(webView: model.webView)
                footer(model)
            }
        }
        .overlay(alignment: .bottom) {
            if !model.selection.isEmpty { lookupBar(model).padding(.bottom, 44) }
        }
        .animation(.easeOut(duration: 0.15), value: model.selection.isEmpty)
        .overlay {
            if let zoomed = model.zoomed {
                ImageZoomView(image: zoomed.image) { model.zoomed = nil }
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.2), value: model.zoomed?.id)
    }

    /// Hiding the controls must not change the size of the page, or the text
    /// would reflow under the reader's eyes: leave the bar's space empty.
    @ViewBuilder
    private func stablePage(_ model: EpubReaderModel, @ViewBuilder content: () -> some View) -> some View {
        #if os(iOS)
        let content = content()
        GeometryReader { geometry in
            content
                .padding(.top, model.controlsVisible ? 0 : max(0, barInset - geometry.safeAreaInsets.top))
                .onChange(of: geometry.safeAreaInsets.top, initial: true) { _, top in
                    if model.controlsVisible { barInset = top }
                }
        }
        #else
        content()
        #endif
    }

    /// Where we are: chapter, page within it, and a hairline for the whole book.
    private func footer(_ model: EpubReaderModel) -> some View {
        VStack(spacing: 4) {
            HStack {
                Text(model.chapterTitle).lineLimit(1)
                Spacer(minLength: 12)
                Text("\(model.page + 1) of \(model.pages)")
                    .monospacedDigit()
                    .contentTransition(.numericText())
                    .animation(.snappy(duration: 0.25), value: model.page)
            }
            .font(.caption)
            .foregroundStyle(.secondary)
            ProgressView(value: min(max(model.progress, 0), 1))
                .tint(.secondary)
                .scaleEffect(y: 0.5)
                .animation(.easeOut(duration: 0.3), value: model.progress)
        }
        .padding(.horizontal, 20)
        .padding(.vertical, 6)
        .accessibilityElement(children: .combine)
    }

    private func lookupBar(_ model: EpubReaderModel) -> some View {
        HStack(spacing: 12) {
            Text("“\(model.selection)”")
                .lineLimit(1)
                .truncationMode(.middle)
                .frame(maxWidth: 180)
            Button("Dictionary", systemImage: "character.book.closed") {
                if let term = model.lookUpInDictionary() { dictionaryTerm = DictionaryTerm(text: term) }
            }
            if let url = model.onlineLookupURL {
                Button("Online", systemImage: "globe") { openURL(url) }
                    .help("Look the word up on Wiktionary in your browser")
            }
        }
        .font(.callout)
        .buttonStyle(.bordered)
        .padding(.horizontal, 14)
        .padding(.vertical, 8)
        .background(.regularMaterial, in: Capsule())
        .shadow(color: .black.opacity(0.15), radius: 8, y: 2)
        .transition(.move(edge: .bottom).combined(with: .opacity))
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        #if os(iOS)
        ToolbarItem(placement: .cancellationAction) {
            Button("Library", systemImage: "books.vertical") { dismiss() }
        }
        #else
        ToolbarItemGroup(placement: .primaryAction) {
            Button("Previous Page", systemImage: "arrowtriangle.left") { model?.previousPage() }
            Button("Next Page", systemImage: "arrowtriangle.right") { model?.nextPage() }
        }
        #endif
        ToolbarItemGroup(placement: .primaryAction) {
            if let model, !model.toc.isEmpty {
                Menu("Contents", systemImage: "list.bullet") {
                    ForEach(Array(model.toc.enumerated()), id: \.offset) { _, entry in
                        Button(String(repeating: "    ", count: Int(min(entry.depth, 4))) + entry.title) { model.go(to: entry) }
                            .disabled(entry.spineIndex == nil)
                    }
                }
            }
            Button("Appearance", systemImage: "textformat.size") { showingSettings = true }
                .popover(isPresented: $showingSettings) {
                    ReaderSettingsView()
                        .presentationCompactAdaptation(.popover)
                }
        }
    }
}

#if os(macOS)
struct WebViewHost: NSViewRepresentable {
    let webView: WKWebView
    func makeNSView(context: Context) -> WKWebView { webView }
    func updateNSView(_ view: WKWebView, context: Context) {}
}
#else
struct WebViewHost: UIViewRepresentable {
    let webView: WKWebView
    func makeUIView(context: Context) -> WKWebView { webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}

/// The system dictionary, as a sheet.
struct DictionaryView: UIViewControllerRepresentable {
    let term: String
    func makeUIViewController(context: Context) -> UIReferenceLibraryViewController {
        UIReferenceLibraryViewController(term: term)
    }
    func updateUIViewController(_ controller: UIReferenceLibraryViewController, context: Context) {}
}
#endif

struct DictionaryTerm: Identifiable {
    let text: String
    var id: String { text }
}
