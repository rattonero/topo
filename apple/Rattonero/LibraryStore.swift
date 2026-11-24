import SwiftUI

extension ShelfOrder: CaseIterable, Identifiable {
    public static var allCases: [ShelfOrder] { [.author, .title, .publisher, .year] }
    public var id: Self { self }

    var label: String {
        switch self {
        case .author: "Author"
        case .title: "Title"
        case .publisher: "Publisher"
        case .year: "Year"
        }
    }
}

/// The shared folder of books and what is on its shelves.
@MainActor
@Observable
final class LibraryStore {
    private(set) var folderURL: URL?
    private(set) var library: Library?
    private(set) var shelves: [Shelf] = []
    /// Books in progress, most recently read first.
    private(set) var continueReading: [BookInfo] = []
    /// False when the folder is read-only and progress stays on this device.
    private(set) var sharesProgress = true
    private(set) var bookCount = 0
    private(set) var isScanning = false
    /// Bumped when reading positions change, so progress marks redraw.
    private(set) var positionsRevision = 0
    var errorMessage: String?

    var order: ShelfOrder {
        didSet {
            UserDefaults.standard.set(order.label, forKey: Keys.order)
            shelves = arrange(books: books, order: order)
        }
    }

    private var books: [BookInfo] = []
    @ObservationIgnored private var isReloadingProgress = false
    private var scopedURL: URL?

    private enum Keys {
        static let bookmark = "library.bookmark"
        static let order = "library.order"
    }

    init() {
        let saved = UserDefaults.standard.string(forKey: Keys.order)
        order = ShelfOrder.allCases.first { $0.label == saved } ?? .author
        restoreFolder()
    }

    /// Open the folder chosen in the file picker and remember it.
    func chooseFolder(_ url: URL) {
        open(url, remember: true)
    }

    func refresh() async {
        guard let library, !isScanning else { return }
        isScanning = true
        let folder = folderURL
        let found = await Task.detached(priority: .userInitiated) {
            Self.fetchSharedProgress(in: folder)
            _ = library.reloadProgress()
            return library.scan()
        }.value
        // The folder may have been switched while the scan ran.
        if library === self.library {
            books = found
            bookCount = found.count
            shelves = arrange(books: found, order: order)
            progressChanged()
        }
        isScanning = false
    }

    /// Reading progress changed here: redraw marks and the top shelf.
    func progressChanged() {
        positionsRevision += 1
        continueReading = library?.continueReading(books: books, limit: 12) ?? []
        sharesProgress = library?.sharesProgress() ?? true
    }

    /// A book was closed, or the app is leaving the foreground: make sure
    /// other devices can see where we are.
    func shareProgress() {
        library?.flush()
        progressChanged()
    }

    /// Pick up what other devices have read since we last looked. Called
    /// every few seconds while the app is in front, so another device's page
    /// turns show up here without doing anything.
    func reloadProgress() async {
        guard let library, !isReloadingProgress else { return }
        isReloadingProgress = true
        defer { isReloadingProgress = false }
        let folder = folderURL
        let changed = await Task.detached(priority: .utility) {
            Self.fetchSharedProgress(in: folder)
            return library.reloadProgress()
        }.value
        if changed, library === self.library { progressChanged() }
    }

    /// Ask the file provider (iCloud Drive and the like) to bring the other
    /// devices' progress files up to date; they are not always downloaded
    /// by themselves, least of all on iOS.
    nonisolated private static func fetchSharedProgress(in folder: URL?) {
        guard let directory = folder?.appendingPathComponent(".rattonero", isDirectory: true) else { return }
        var error: NSError?
        NSFileCoordinator(filePresenter: nil).coordinate(readingItemAt: directory, options: .withoutChanges, error: &error) { _ in }
        let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        for file in files {
            try? FileManager.default.startDownloadingUbiquitousItem(at: file)
        }
    }

    func dismissFromContinueReading(_ book: BookInfo) {
        library?.dismissFromContinueReading(bookId: book.id)
        progressChanged()
    }

    func progress(of book: BookInfo) -> Double? {
        _ = positionsRevision
        return library?.position(bookId: book.id)?.progress
    }

    func book(withID id: String) -> BookInfo? {
        books.first { $0.id == id }
    }

    // MARK: - Folder access

    private func restoreFolder() {
        #if DEBUG
        // Development hook: point the app at a folder without the picker.
        if let path = ProcessInfo.processInfo.environment["RATTONERO_LIBRARY"] {
            open(URL(fileURLWithPath: path, isDirectory: true), remember: false)
            return
        }
        #endif
        if let data = UserDefaults.standard.data(forKey: Keys.bookmark) {
            var stale = false
            if let url = try? URL(resolvingBookmarkData: data, options: Self.resolveOptions, bookmarkDataIsStale: &stale) {
                open(url, remember: stale)
                if library != nil { return }
            }
        }
        #if os(iOS)
        // On iOS the app's own Documents folder (visible in Files) is the
        // default library until another folder is chosen.
        if let documents = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first {
            open(documents, remember: false)
        }
        #endif
    }

    private func open(_ url: URL, remember: Bool) {
        scopedURL?.stopAccessingSecurityScopedResource()
        scopedURL = url.startAccessingSecurityScopedResource() ? url : nil
        do {
            let support = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let state = support.appendingPathComponent("Rattonero", isDirectory: true)
            library = try Library(folder: url.path, stateDir: state.path)
            folderURL = url
            errorMessage = nil
            if remember, let data = try? url.bookmarkData(options: Self.bookmarkOptions) {
                UserDefaults.standard.set(data, forKey: Keys.bookmark)
            }
            books = []
            shelves = []
            continueReading = []
            bookCount = 0
            Task { await refresh() }
        } catch {
            errorMessage = describe(error)
        }
    }

    #if os(macOS)
    private static let bookmarkOptions: URL.BookmarkCreationOptions = [.withSecurityScope]
    private static let resolveOptions: URL.BookmarkResolutionOptions = [.withSecurityScope]
    #else
    private static let bookmarkOptions: URL.BookmarkCreationOptions = []
    private static let resolveOptions: URL.BookmarkResolutionOptions = []
    #endif
}
