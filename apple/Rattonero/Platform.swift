import SwiftUI

#if os(macOS)
import AppKit
typealias PlatformImage = NSImage
#else
import UIKit
typealias PlatformImage = UIImage
#endif

extension Image {
    init(platformImage: PlatformImage) {
        #if os(macOS)
        self.init(nsImage: platformImage)
        #else
        self.init(uiImage: platformImage)
        #endif
    }
}

extension BookInfo: Identifiable {}

extension Shelf: Identifiable {
    public var id: String { label }
}

extension BookInfo {
    var authorLine: String { authors.joined(separator: ", ") }
}

/// A message fit for showing to the reader.
func describe(_ error: Error) -> String {
    guard let error = error as? CoreError else { return error.localizedDescription }
    switch error {
    case .Io(let message): return message
    case .Invalid(let message): return "This file is not a readable book (\(message))."
    case .NotFound(let message): return "Not found: \(message)"
    case .Protected: return "This book is protected by DRM and cannot be opened."
    }
}
