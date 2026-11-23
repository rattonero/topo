import SwiftUI
import UniformTypeIdentifiers

/// The main view: every book in the shared folder, on shelves.
struct BookshelfView: View {
    @Environment(LibraryStore.self) private var store
    @State private var openBook: BookInfo?
    @State private var choosingFolder = false

    var body: some View {
        @Bindable var store = store
        NavigationStack {
            content
                .navigationTitle("Rattonero Books")
                #if os(macOS)
                .navigationSubtitle(subtitle)
                .navigationDestination(item: $openBook) { book in
                    ReaderView(book: book)
                }
                #else
                .navigationBarTitleDisplayMode(.inline)
                #endif
                .toolbar { toolbar }
        }
        #if os(iOS)
        .fullScreenCover(item: $openBook) { book in
            NavigationStack { ReaderView(book: book) }
        }
        #endif
        .onChange(of: openBook) { _, book in
            if book == nil { store.positionsChanged() }
        }
        .fileImporter(isPresented: $choosingFolder, allowedContentTypes: [.folder]) { result in
            if case .success(let url) = result { store.chooseFolder(url) }
        }
        .alert("Could not open the folder", isPresented: .constant(store.errorMessage != nil)) {
            Button("OK") { store.errorMessage = nil }
        } message: {
            Text(store.errorMessage ?? "")
        }
        #if DEBUG
        .task {
            // Development hook: open a book straight away, by library id.
            guard let id = ProcessInfo.processInfo.environment["RATTONERO_OPEN"] else { return }
            for _ in 0..<50 where openBook == nil {
                try? await Task.sleep(for: .milliseconds(100))
                openBook = store.book(withID: id)
            }
        }
        #endif
    }

    private var subtitle: String {
        guard store.folderURL != nil else { return "" }
        return store.bookCount == 1 ? "1 book" : "\(store.bookCount) books"
    }

    @ViewBuilder
    private var content: some View {
        if store.folderURL == nil {
            ContentUnavailableView {
                Label("Choose your library folder", systemImage: "books.vertical")
            } description: {
                Text("Rattonero Books shows every EPUB and PDF in a folder you pick, such as one shared through iCloud Drive or a network disk. It only ever reads from it.")
            } actions: {
                Button("Choose Folder…") { choosingFolder = true }
                    .buttonStyle(.borderedProminent)
            }
        } else if store.shelves.isEmpty {
            if store.isScanning {
                ProgressView("Looking for books…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ContentUnavailableView {
                    Label("No books here yet", systemImage: "books.vertical")
                } description: {
                    Text("Put EPUB or PDF files in “\(store.folderURL?.lastPathComponent ?? "")” and they will appear on the shelf.")
                } actions: {
                    Button("Look Again") { Task { await store.refresh() } }
                    Button("Choose Another Folder…") { choosingFolder = true }
                }
            }
        } else {
            GeometryReader { proxy in
                let columns = max(2, Int((proxy.size.width - 24) / 132))
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(rows(columns: columns), id: \.first?.book.id) { row in
                            ShelfRow(places: row, columns: columns) { openBook = $0 }
                        }
                    }
                    .padding(.bottom, 24)
                }
                .refreshable { await store.refresh() }
            }
            .background(Color.shelfWall)
        }
    }

    /// Books run on from shelf to shelf, as in a library; the first book of
    /// each group carries the group's label.
    private func rows(columns: Int) -> [[ShelfPlace]] {
        let places = store.shelves.flatMap { shelf in
            shelf.books.enumerated().map { ShelfPlace(book: $1, label: $0 == 0 ? shelf.label : nil) }
        }
        return stride(from: 0, to: places.count, by: columns).map { Array(places[$0..<min($0 + columns, places.count)]) }
    }

    @ToolbarContentBuilder
    private var toolbar: some ToolbarContent {
        @Bindable var store = store
        ToolbarItem(placement: .primaryAction) {
            Picker("Arrange by", selection: $store.order) {
                ForEach(ShelfOrder.allCases) { Text($0.label).tag($0) }
            }
            #if os(macOS)
            .pickerStyle(.segmented)
            #else
            .pickerStyle(.menu)
            #endif
            .help("Arrange the shelves by author, title, publisher or year")
        }
        ToolbarItem(placement: .automatic) {
            Menu {
                Button("Choose Library Folder…", systemImage: "folder") { choosingFolder = true }
                Button("Look for New Books", systemImage: "arrow.clockwise") { Task { await store.refresh() } }
                    .keyboardShortcut("r")
                    .disabled(store.library == nil)
                if let folder = store.folderURL {
                    Divider()
                    Text(folder.path(percentEncoded: false))
                }
            } label: {
                Label("Library", systemImage: "folder")
            }
        }
    }
}

private extension Color {
    static let shelfWall = Color("ShelfWall", bundle: nil)
}

struct ShelfPlace: Identifiable {
    let book: BookInfo
    /// Set on the first book of a group: "C", "Einaudi", "1957".
    let label: String?
    var id: String { book.id }
}

/// The little label a library puts at the start of a run of books.
private struct ShelfLabel: View {
    let text: String?

    var body: some View {
        Text(text ?? " ")
            .font(.system(.caption, design: .serif).weight(.semibold))
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .padding(.horizontal, 7)
            .padding(.vertical, 2)
            .background(.quaternary.opacity(text == nil ? 0 : 1), in: Capsule())
            .padding(.bottom, 6)
            .accessibilityAddTraits(text == nil ? [] : .isHeader)
            .accessibilityHidden(text == nil)
    }
}

/// One shelf: covers standing on a wooden board, captions beneath.
private struct ShelfRow: View {
    @Environment(LibraryStore.self) private var store
    let places: [ShelfPlace]
    let columns: Int
    let open: (BookInfo) -> Void

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: 0) {
                cells { place in
                    let book = place.book
                    VStack(spacing: 0) {
                        ShelfLabel(text: place.label)
                        Button { open(book) } label: {
                            BookCoverView(book: book, library: store.library)
                                .overlay(alignment: .topTrailing) {
                                    if book.format == .pdf { badge("PDF") }
                                }
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("\(book.title), \(book.authorLine)")
                        .help(tooltip(for: book))
                    }
                }
            }
            .padding(.top, 16)

            Board()

            HStack(alignment: .top, spacing: 0) {
                cells { place in
                    let book = place.book
                    VStack(spacing: 2) {
                        if let progress = store.progress(of: book), progress > 0.005 {
                            ProgressView(value: min(progress, 1))
                                .tint(.secondary)
                                .frame(width: 70)
                                .scaleEffect(y: 0.6)
                        }
                        Text(book.title)
                            .font(.caption.weight(.medium))
                            .lineLimit(2)
                        Text(book.authorLine)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 6)
                    .accessibilityHidden(true)
                }
            }
            .padding(.top, 8)
        }
        .padding(.horizontal, 12)
        .padding(.bottom, 6)
    }

    /// Equal-width cells, padded with blanks so short rows stay aligned.
    @ViewBuilder
    private func cells(@ViewBuilder _ cell: @escaping (ShelfPlace) -> some View) -> some View {
        ForEach(places) { place in
            cell(place).frame(maxWidth: .infinity)
        }
        ForEach(0..<max(0, columns - places.count), id: \.self) { _ in
            Color.clear.frame(maxWidth: .infinity, maxHeight: 1)
        }
    }

    private func badge(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 9, weight: .bold))
            .padding(.horizontal, 4)
            .padding(.vertical, 2)
            .background(.black.opacity(0.65), in: RoundedRectangle(cornerRadius: 3))
            .foregroundStyle(.white)
            .padding(4)
    }

    private func tooltip(for book: BookInfo) -> String {
        var parts = [book.title, book.authorLine]
        if let publisher = book.publisher { parts.append(publisher) }
        if let year = book.year { parts.append(String(year)) }
        return parts.filter { !$0.isEmpty }.joined(separator: "\n")
    }
}

private struct Board: View {
    var body: some View {
        VStack(spacing: 0) {
            Rectangle().fill(Color("ShelfBoardTop", bundle: nil)).frame(height: 5)
            Rectangle().fill(Color("ShelfBoardFront", bundle: nil)).frame(height: 9)
        }
        .shadow(color: .black.opacity(0.25), radius: 4, y: 4)
    }
}
