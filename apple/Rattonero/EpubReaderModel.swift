import SwiftUI
import WebKit

/// Where to land when a chapter opens.
enum ChapterStart {
    case fraction(Double)
    case anchor(String)
    case end

    var json: String {
        switch self {
        case .fraction(let value): return "{\"fraction\":\(value)}"
        case .end: return "{\"end\":true}"
        case .anchor(let id):
            let data = (try? JSONEncoder().encode(id)) ?? Data("\"\"".utf8)
            return "{\"anchor\":\(String(decoding: data, as: UTF8.self))}"
        }
    }
}

struct ZoomedImage: Identifiable {
    let id = UUID()
    let image: PlatformImage
}

/// Drives one open EPUB: a locked-down web view fed by the Rust core.
///
/// The web view never touches the file system or the network. Every request
/// goes through `rattonero-book://`, answered from the core, which hands out
/// sanitised chapters. Book content cannot run scripts; only the core's own
/// pagination script runs, injected here as a user script.
@MainActor
@Observable
final class EpubReaderModel: NSObject, WKNavigationDelegate {
    let info: BookInfo
    let toc: [TocEntry]
    let webView: WKWebView

    private(set) var spineIndex = 0
    private(set) var page = 0
    private(set) var pages = 1
    private(set) var selection = ""
    private(set) var selectionRect = CGRect.zero
    var zoomed: ZoomedImage?
    var controlsVisible = true

    @ObservationIgnored private let book: EpubBook
    @ObservationIgnored private let library: Library?
    @ObservationIgnored private let spineCount: Int
    @ObservationIgnored private var settingsJSON = "{}"
    @ObservationIgnored private let bridge = Bridge()
    @ObservationIgnored var openExternal: (URL) -> Void = { _ in }

    private static let scheme = urlScheme()
    private static let script = readerScript()
    private static let policy = contentSecurityPolicy()

    init(info: BookInfo, library: Library?, settingsJSON: String) throws {
        self.info = info
        self.library = library
        self.settingsJSON = settingsJSON
        book = try EpubBook.open(path: info.path)
        toc = book.toc()
        spineCount = Int(book.spineCount())

        let configuration = WKWebViewConfiguration()
        configuration.websiteDataStore = .nonPersistent()
        configuration.defaultWebpagePreferences.allowsContentJavaScript = false
        configuration.setURLSchemeHandler(bridge, forURLScheme: Self.scheme)
        configuration.userContentController.add(bridge, name: "rattonero")
        #if os(iOS)
        configuration.dataDetectorTypes = []
        #endif
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()

        bridge.model = self
        webView.navigationDelegate = self
        webView.allowsLinkPreview = false
        #if os(macOS)
        webView.setValue(false, forKey: "drawsBackground")
        #else
        webView.isOpaque = false
        webView.backgroundColor = .clear
        webView.scrollView.backgroundColor = .clear
        webView.scrollView.isScrollEnabled = false
        webView.scrollView.bounces = false
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        #endif

    }

    /// Install the network block, then open the book where it was left.
    func start() async {
        if let rules = await Self.offlineRules.value {
            webView.configuration.userContentController.add(rules)
        }
        if let saved = library?.position(bookId: info.id), Int(saved.index) < spineCount {
            load(Int(saved.index), at: .fraction(saved.fraction))
        } else {
            load(0, at: .fraction(0))
        }
    }

    /// A content blocker that refuses every load except the book's own
    /// resources: a second lock behind the Content-Security-Policy.
    private static let offlineRules = Task<WKContentRuleList?, Never> { @MainActor in
        let rules = """
        [{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
         {"trigger":{"url-filter":"^\(scheme):"},"action":{"type":"ignore-previous-rules"}},
         {"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}}]
        """
        return try? await WKContentRuleListStore.default().compileContentRuleList(forIdentifier: "rattonero-offline", encodedContentRuleList: rules)
    }

    // MARK: - Navigation

    var chapterTitle: String {
        toc.last { ($0.spineIndex).map { Int($0) <= spineIndex } ?? false }?.title ?? info.title
    }

    var progress: Double {
        guard spineCount > 0 else { return 0 }
        return (Double(spineIndex) + Double(page) / Double(max(pages, 1))) / Double(spineCount)
    }

    func nextPage() { run("Rattonero.next()") }
    func previousPage() { run("Rattonero.prev()") }

    func go(to entry: TocEntry) {
        guard let index = entry.spineIndex else { return }
        load(Int(index), at: entry.fragment.map(ChapterStart.anchor) ?? .fraction(0))
    }

    func apply(settingsJSON: String) {
        self.settingsJSON = settingsJSON
        run("Rattonero.apply(\(settingsJSON))")
    }

    private func run(_ script: String) {
        webView.evaluateJavaScript("window.Rattonero && \(script); null", completionHandler: nil)
    }

    private func load(_ index: Int, at start: ChapterStart) {
        guard index >= 0, index < spineCount, let path = book.spinePath(index: UInt32(index)) else { return }
        spineIndex = index
        selection = ""
        // The pagination script runs before any content so the page is laid
        // out, themed and positioned before it is first shown.
        let controller = webView.configuration.userContentController
        controller.removeAllUserScripts()
        let source = Self.script + "\nRattonero.configure(\(settingsJSON), \(start.json));"
        controller.addUserScript(WKUserScript(source: source, injectionTime: .atDocumentStart, forMainFrameOnly: true))

        var components = URLComponents()
        components.scheme = Self.scheme
        components.host = "book"
        components.path = "/" + path
        if let url = components.url {
            webView.load(URLRequest(url: url))
        }
    }

    private func follow(_ url: URL) {
        if url.scheme == Self.scheme {
            guard let index = book.spineIndexOf(path: url.path).map(Int.init) else { return }
            let fragment = url.fragment?.removingPercentEncoding
            if index == spineIndex, let fragment {
                let id = ChapterStart.anchor(fragment).json
                run("Rattonero.goToAnchor(\(id).anchor)")
            } else {
                load(index, at: fragment.map(ChapterStart.anchor) ?? .fraction(0))
            }
        } else if ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? "") {
            openExternal(url)
        }
    }

    private func savePosition() {
        let fraction = Double(page) / Double(max(pages, 1))
        library?.setPosition(bookId: info.id, position: ReadingPosition(index: UInt32(spineIndex), fraction: fraction, progress: progress, finished: spineIndex == spineCount - 1 && page >= pages - 1))
    }

    // MARK: - Messages from the pagination script

    fileprivate func receive(_ body: Any) {
        guard let message = body as? [String: Any], let type = message["type"] as? String else { return }
        switch type {
        case "position":
            page = (message["page"] as? NSNumber)?.intValue ?? 0
            pages = max(1, (message["pages"] as? NSNumber)?.intValue ?? 1)
            savePosition()
        case "edge":
            if message["direction"] as? String == "next" {
                load(spineIndex + 1, at: .fraction(0))
            } else {
                load(spineIndex - 1, at: .end)
            }
        case "link":
            if let href = message["href"] as? String, let url = URL(string: href) { follow(url) }
        case "image":
            if let src = message["src"] as? String, let url = URL(string: src), url.scheme == Self.scheme,
               let data = try? book.resource(path: url.path).data, let image = PlatformImage(data: data) {
                zoomed = ZoomedImage(image: image)
            }
        case "tap":
            controlsVisible.toggle()
        case "selection":
            selection = message["text"] as? String ?? ""
            if let rect = message["rect"] as? [String: NSNumber] {
                selectionRect = CGRect(x: rect["x"]?.doubleValue ?? 0, y: rect["y"]?.doubleValue ?? 0,
                                       width: rect["width"]?.doubleValue ?? 0, height: rect["height"]?.doubleValue ?? 0)
            }
        default:
            break
        }
    }

    // MARK: - Serving the book

    fileprivate func serve(_ task: WKURLSchemeTask) {
        guard let url = task.request.url else {
            task.didFailWithError(URLError(.badURL))
            return
        }
        do {
            let resource = try book.resource(path: url.path)
            var headers = [
                "Content-Type": resource.mime,
                "Content-Length": String(resource.data.count),
                "Cache-Control": "no-store",
            ]
            if resource.mime == "text/html" {
                headers["Content-Type"] = "text/html; charset=utf-8"
                headers["Content-Security-Policy"] = Self.policy
            }
            guard let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers) else {
                task.didFailWithError(URLError(.badServerResponse))
                return
            }
            task.didReceive(response)
            task.didReceive(resource.data)
            task.didFinish()
        } catch {
            task.didFailWithError(URLError(.fileDoesNotExist))
        }
    }

    // MARK: - WKNavigationDelegate

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        // Only chapters we load ourselves; never a navigation the content asks for.
        let ours = action.request.url?.scheme == Self.scheme && action.navigationType == .other
        return ours && action.targetFrame?.isMainFrame == true ? .allow : .cancel
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        #if os(macOS)
        // Arrow keys and the space bar turn pages straight away.
        webView.window?.makeFirstResponder(webView)
        #endif
    }

    // MARK: - Word lookup

    /// Show the system dictionary for the selected text.
    func lookUpInDictionary() -> String? {
        guard !selection.isEmpty else { return nil }
        #if os(macOS)
        let text = NSAttributedString(string: selection, attributes: [.font: NSFont.systemFont(ofSize: 16)])
        webView.showDefinition(for: text, at: CGPoint(x: selectionRect.minX, y: selectionRect.maxY - 4))
        return nil
        #else
        return selection
        #endif
    }

    /// A dictionary page for the selection, in the book's language.
    var onlineLookupURL: URL? {
        guard !selection.isEmpty else { return nil }
        var language = String((info.language ?? "en").prefix(2)).lowercased()
        if language.count != 2 || !language.allSatisfy({ $0.isASCII && $0.isLetter }) { language = "en" }
        var components = URLComponents(string: "https://\(language).wiktionary.org/w/index.php")
        components?.queryItems = [URLQueryItem(name: "search", value: selection.lowercased())]
        return components?.url
    }
}

/// Breaks the retain cycle between the web view configuration and the model.
private final class Bridge: NSObject, WKScriptMessageHandler, WKURLSchemeHandler {
    weak var model: EpubReaderModel?

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard message.frameInfo.isMainFrame else { return }
        model?.receive(message.body)
    }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        if let model {
            model.serve(task)
        } else {
            task.didFailWithError(URLError(.cancelled))
        }
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {}
}

