import SwiftUI

@main
struct RattoneroApp: App {
    @State private var store = LibraryStore()
    @State private var settings = ReaderSettings()

    var body: some Scene {
        WindowGroup {
            BookshelfView()
                .environment(store)
                .environment(settings)
        }
        #if os(macOS)
        .defaultSize(width: 1040, height: 760)
        #endif
    }
}
