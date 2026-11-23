import SwiftUI
import ImageIO
import CoreGraphics

/// Loads and caches cover thumbnails off the main thread. EPUB covers come
/// from the core; PDF covers are the first page, drawn by the platform.
actor CoverLoader {
    static let shared = CoverLoader()

    private var cache: [String: CGImage] = [:]
    private var missing: Set<String> = []
    private let maxPixels = 520

    func cover(for book: BookInfo, in library: Library) -> CGImage? {
        let key = book.path
        if let image = cache[key] { return image }
        if missing.contains(key) { return nil }
        let image: CGImage?
        switch book.format {
        case .epub:
            image = book.hasCover ? library.cover(bookId: book.id).flatMap(thumbnail(from:)) : nil
        case .pdf:
            image = firstPage(ofPDFAt: URL(fileURLWithPath: book.path))
        }
        if let image {
            if cache.count > 600 { cache.removeAll() }
            cache[key] = image
        } else {
            missing.insert(key)
        }
        return image
    }

    private func thumbnail(from data: Data) -> CGImage? {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixels,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
    }

    private func firstPage(ofPDFAt url: URL) -> CGImage? {
        guard let document = CGPDFDocument(url as CFURL), let page = document.page(at: 1) else { return nil }
        let box = page.getBoxRect(.cropBox)
        guard box.width > 1, box.height > 1 else { return nil }
        let scale = CGFloat(maxPixels) / max(box.width, box.height)
        let size = CGSize(width: (box.width * scale).rounded(), height: (box.height * scale).rounded())
        guard let context = CGContext(
            data: nil, width: Int(size.width), height: Int(size.height), bitsPerComponent: 8, bytesPerRow: 0,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }
        context.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        context.fill(CGRect(origin: .zero, size: size))
        context.scaleBy(x: scale, y: scale)
        context.translateBy(x: -box.minX, y: -box.minY)
        context.drawPDFPage(page)
        return context.makeImage()
    }
}

/// A book standing on the shelf, cover out.
struct BookCoverView: View {
    let book: BookInfo
    let library: Library?
    /// Covers shrink on narrow screens so a shelf always holds three books.
    var width: CGFloat = BookCoverView.fullWidth
    @State private var cover: CGImage?

    static let fullWidth: CGFloat = 112
    private var size: CGSize { CGSize(width: width, height: (width * 164 / 112).rounded()) }
    private var scale: CGFloat { width / Self.fullWidth }

    var body: some View {
        Group {
            if let cover {
                Image(decorative: cover, scale: 1)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
            } else {
                placeholder
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 3))
        .overlay(alignment: .leading) {
            // The hint of a spine.
            LinearGradient(colors: [.black.opacity(0.28), .clear], startPoint: .leading, endPoint: .trailing)
                .frame(width: 5)
                .clipShape(RoundedRectangle(cornerRadius: 3))
        }
        .shadow(color: .black.opacity(0.35), radius: 3, x: 2, y: 1)
        .frame(width: size.width, height: size.height, alignment: .bottom)
        .task(id: book.path) {
            guard let library else { return }
            cover = await CoverLoader.shared.cover(for: book, in: library)
        }
    }

    /// A plain cloth binding with the title, for books without a cover.
    private var placeholder: some View {
        let hue = Double(abs(book.title.unicodeScalars.reduce(7) { ($0 &* 31 &+ Int($1.value)) % 360 })) / 360
        return ZStack {
            Color(hue: hue, saturation: 0.42, brightness: 0.46)
            VStack(spacing: 6) {
                Text(book.title)
                    .font(.system(size: 13 * scale, weight: .semibold, design: .serif))
                    .lineLimit(5)
                Rectangle().frame(width: 24, height: 1).opacity(0.6)
                Text(book.authorLine)
                    .font(.system(size: 10 * scale, design: .serif))
                    .lineLimit(2)
                    .opacity(0.85)
            }
            .multilineTextAlignment(.center)
            .foregroundStyle(.white)
            .padding(10 * scale)
        }
        .frame(width: size.width, height: size.height)
    }
}
