import SwiftUI
import PDFKit

/// PDFs are shown a page at a time by PDFKit; the core only keeps the place.
struct PDFReaderView: View {
    let book: BookInfo
    @Environment(LibraryStore.self) private var store
    @Environment(\.dismiss) private var dismiss
    @State private var document: PDFDocument?
    @State private var failed = false
    @State private var pageIndex = 0
    @State private var turner = PDFPageTurner()

    var body: some View {
        Group {
            if let document {
                VStack(spacing: 0) {
                    PDFPageView(document: document, pageIndex: $pageIndex, turner: turner)
                    HStack {
                        Spacer()
                        Text("\(pageIndex + 1) of \(document.pageCount)")
                            .monospacedDigit()
                            .contentTransition(.numericText())
                            .animation(.snappy(duration: 0.25), value: pageIndex)
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 20)
                    .padding(.vertical, 6)
                }
            } else if failed {
                ContentUnavailableView("This PDF cannot be opened", systemImage: "doc.questionmark")
            } else {
                ProgressView()
            }
        }
        .navigationTitle(book.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        #endif
        .toolbar {
            #if os(iOS)
            ToolbarItem(placement: .cancellationAction) {
                Button("Library", systemImage: "books.vertical") { dismiss() }
            }
            #else
            ToolbarItemGroup(placement: .primaryAction) {
                Button("Previous Page", systemImage: "arrowtriangle.left") { turner.turn(forward: false) }
                    .keyboardShortcut(.leftArrow, modifiers: [])
                Button("Next Page", systemImage: "arrowtriangle.right") { turner.turn(forward: true) }
                    .keyboardShortcut(.rightArrow, modifiers: [])
            }
            #endif
        }
        .task {
            guard document == nil else { return }
            if let loaded = PDFDocument(url: URL(fileURLWithPath: book.path)), loaded.pageCount > 0, !loaded.isLocked {
                let saved = Int(store.library?.position(bookId: book.id)?.index ?? 0)
                pageIndex = min(saved, loaded.pageCount - 1)
                document = loaded
            } else {
                failed = true
            }
        }
        .onChange(of: pageIndex) { _, index in
            guard let document else { return }
            let progress = Double(index + 1) / Double(document.pageCount)
            store.library?.setPosition(bookId: book.id, position: ReadingPosition(index: UInt32(index), fraction: 0, progress: progress, finished: index >= document.pageCount - 1))
        }
    }
}

/// Lets toolbar buttons turn the page of the PDF view.
@MainActor
final class PDFPageTurner {
    weak var view: PDFView?

    func turn(forward: Bool) {
        guard let view, forward ? view.canGoToNextPage : view.canGoToPreviousPage else { return }
        #if os(macOS)
        // Slide the new page in, so a turn reads as a turn.
        let slide = CATransition()
        slide.type = .push
        slide.subtype = forward ? .fromRight : .fromLeft
        slide.duration = 0.28
        slide.timingFunction = CAMediaTimingFunction(name: .easeOut)
        view.layer?.add(slide, forKey: "pageTurn")
        #endif
        if forward { view.goToNextPage(nil) } else { view.goToPreviousPage(nil) }
    }
}

@MainActor
private func configure(_ view: PDFView, document: PDFDocument, pageIndex: Int, coordinator: PDFPageView.Coordinator) {
    view.displayMode = .singlePage
    view.displaysPageBreaks = false
    view.autoScales = true
    #if os(iOS)
    view.displayDirection = .horizontal
    view.usePageViewController(true, withViewOptions: nil)
    #else
    view.wantsLayer = true
    #endif
    view.document = document
    NotificationCenter.default.addObserver(coordinator, selector: #selector(PDFPageView.Coordinator.pageChanged(_:)), name: .PDFViewPageChanged, object: view)
    if let page = document.page(at: pageIndex) {
        // The page view controller needs a layout pass before it can jump.
        DispatchQueue.main.async { view.go(to: page) }
    }
}

#if os(macOS)
struct PDFPageView: NSViewRepresentable {
    let document: PDFDocument
    @Binding var pageIndex: Int
    let turner: PDFPageTurner

    func makeCoordinator() -> Coordinator { Coordinator(pageIndex: $pageIndex) }
    func makeNSView(context: Context) -> PDFView {
        let view = PDFView()
        configure(view, document: document, pageIndex: pageIndex, coordinator: context.coordinator)
        turner.view = view
        return view
    }
    func updateNSView(_ view: PDFView, context: Context) {}
}
#else
struct PDFPageView: UIViewRepresentable {
    let document: PDFDocument
    @Binding var pageIndex: Int
    let turner: PDFPageTurner

    func makeCoordinator() -> Coordinator { Coordinator(pageIndex: $pageIndex) }
    func makeUIView(context: Context) -> PDFView {
        let view = PDFView()
        configure(view, document: document, pageIndex: pageIndex, coordinator: context.coordinator)
        turner.view = view
        return view
    }
    func updateUIView(_ view: PDFView, context: Context) {}
}
#endif

extension PDFPageView {
    @MainActor
    final class Coordinator: NSObject {
        @Binding var pageIndex: Int
        init(pageIndex: Binding<Int>) { _pageIndex = pageIndex }

        @objc func pageChanged(_ note: Notification) {
            guard let view = note.object as? PDFView, let page = view.currentPage, let document = view.document else { return }
            let index = document.index(for: page)
            if index != NSNotFound, index != pageIndex { pageIndex = index }
        }
    }
}
