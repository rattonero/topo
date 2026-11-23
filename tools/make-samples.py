#!/usr/bin/env python3
"""Writes a few small, made-up books for trying the reader out.

Usage: tools/make-samples.py <output folder>
"""
import struct, sys, zlib, zipfile, pathlib

out = pathlib.Path(sys.argv[1]); out.mkdir(parents=True, exist_ok=True)

def png(w, h, rgb1, rgb2):
    rows = b""
    for y in range(h):
        t = y / max(h - 1, 1)
        px = bytes(int(a + (b - a) * t) for a, b in zip(rgb1, rgb2))
        row = bytearray()
        for x in range(w):
            ring = ((x - w / 2) ** 2 + (y - h / 2) ** 2) ** 0.5
            row += bytes(min(255, c + 60) for c in px) if int(ring) % 40 < 3 else px
        rows += b"\0" + bytes(row)
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")

WORDS = {
    "en": "the quiet river carried small boats past the old library where nobody hurried and every window held a different afternoon light while the keeper counted pages and listened for rain".split(),
    "it": "il fiume tranquillo portava piccole barche oltre la vecchia biblioteca dove nessuno aveva fretta e ogni finestra custodiva una luce diversa mentre il custode contava le pagine e ascoltava la pioggia".split(),
}

def prose(lang, seed, paragraphs):
    words, out, n = WORDS[lang], [], seed
    for _ in range(paragraphs):
        sentences = []
        for _ in range(5 + n % 4):
            length = 9 + n % 11
            s = " ".join(words[(n * 7 + i * (3 + n % 5)) % len(words)] for i in range(length))
            sentences.append(s.capitalize() + ".")
            n = (n * 1103515245 + 12345) % 2 ** 31
        out.append("<p>" + " ".join(sentences) + "</p>")
    return "\n".join(out)

def epub(name, title, author, publisher, year, lang, chapters, cover=None, file_as=None):
    opf_items, spine, nav = [], [], []
    with zipfile.ZipFile(out / name, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("mimetype", "application/epub+zip", zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml", '<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/book.opf" media-type="application/oebps-package+xml"/></rootfiles></container>')
        z.writestr("OEBPS/style.css", "h1 { text-align: center; margin: 2em 0 1em; } .note { font-size: 0.85em; }")
        z.writestr("OEBPS/images/plate.png", png(360, 240, (40, 70, 120), (200, 150, 90)))
        if cover:
            z.writestr("OEBPS/images/cover.png", png(300, 440, *cover))
            opf_items.append('<item id="cover" href="images/cover.png" media-type="image/png" properties="cover-image"/>')
        for i, chapter in enumerate(chapters, 1):
            extra = ""
            if i == 1:
                extra = ('<p>See the plate below, and a note<a href="ch2.xhtml#note1">[1]</a>. <script>document.title="scripts must never run"</script></p>'
                         '<figure><img src="images/plate.png" alt="A plate" onerror="alert(1)"/><figcaption>Plate I</figcaption></figure>')
            if i == 2:
                extra = '<p class="note" id="note1">[1] A note, reached by a link from the first chapter.</p>'
            body = f'<h1 id="top">{chapter}</h1>\n{prose(lang, i * 17 + len(title), 26)}\n{extra}\n{prose(lang, i * 5 + 3, 12)}'
            z.writestr(f"OEBPS/ch{i}.xhtml", f'<?xml version="1.0" encoding="utf-8"?>\n<html xmlns="http://www.w3.org/1999/xhtml" xml:lang="{lang}"><head><title>{chapter}</title><link rel="stylesheet" href="style.css"/></head><body>\n{body}\n</body></html>')
            opf_items.append(f'<item id="ch{i}" href="ch{i}.xhtml" media-type="application/xhtml+xml"/>')
            spine.append(f'<itemref idref="ch{i}"/>')
            nav.append(f'<li><a href="ch{i}.xhtml">{chapter}</a></li>')
        z.writestr("OEBPS/nav.xhtml", f'<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Contents</title></head><body><nav epub:type="toc"><ol>{"".join(nav)}</ol></nav></body></html>')
        creator = f'<dc:creator id="a">{author}</dc:creator>' + (f'<meta refines="#a" property="file-as">{file_as}</meta>' if file_as else "")
        z.writestr("OEBPS/book.opf", f'''<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
<dc:identifier id="id">urn:rattonero:sample:{name}</dc:identifier><dc:title>{title}</dc:title>{creator}
<dc:publisher>{publisher}</dc:publisher><dc:date>{year}-01-01</dc:date><dc:language>{lang}</dc:language></metadata>
<manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="css" href="style.css" media-type="text/css"/>
<item id="plate" href="images/plate.png" media-type="image/png"/>{"".join(opf_items)}</manifest><spine>{"".join(spine)}</spine></package>''')

def pdf(name, title, author, pages):
    objs = ["<< /Type /Catalog /Pages 2 0 R >>", None, "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>"]
    kids = []
    for n in range(1, pages + 1):
        lines = [f"BT /F1 22 Tf 72 720 Td ({title}) Tj ET", f"BT /F1 12 Tf 72 690 Td (Page {n} of {pages}) Tj ET"]
        lines += [f"BT /F1 11 Tf 72 {650 - 16 * i} Td ({' '.join(WORDS['en'][(i + n + j) % 30] for j in range(12))}) Tj ET" for i in range(34)]
        stream = "\n".join(lines)
        objs.append(f"<< /Length {len(stream)} >>\nstream\n{stream}\nendstream")
        objs.append(f"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {len(objs)} 0 R /Resources << /Font << /F1 3 0 R >> >> >>")
        kids.append(f"{len(objs)} 0 R")
    objs[1] = f"<< /Type /Pages /Kids [{' '.join(kids)}] /Count {pages} >>"
    objs.append(f"<< /Title ({title}) /Author ({author}) /CreationDate (D:20190315120000Z) >>")
    data, offsets = b"%PDF-1.4\n", []
    for i, o in enumerate(objs, 1):
        offsets.append(len(data)); data += f"{i} 0 obj\n{o}\nendobj\n".encode("latin-1")
    xref = len(data)
    data += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode() + b"".join(f"{o:010d} 00000 n \n".encode() for o in offsets)
    data += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R /Info {len(objs)} 0 R >>\nstartxref\n{xref}\n%%EOF\n".encode()
    (out / name).write_bytes(data)

IT = ["Il ponte", "La bottega", "L’inverno", "Il ritorno"]
EN = ["The Bridge", "The Workshop", "Winter", "The Return"]
epub("fiume.epub", "Il fiume tranquillo", "Marta Bellandi", "Edizioni del Ponte", 1962, "it", IT, ((120, 40, 40), (230, 190, 120)))
epub("custode.epub", "Il custode delle pagine", "Marta Bellandi", "Edizioni del Ponte", 1971, "it", IT, ((30, 80, 70), (200, 220, 180)))
epub("keeper.epub", "The Keeper of Windows", "Edmund de Vries", "Harbour Press", 1998, "en", EN, ((40, 50, 110), (180, 200, 240)), file_as="de Vries, Edmund")
epub("rain.epub", "A Season of Rain", "Anna Okafor", "Harbour Press", 2011, "en", EN, ((70, 70, 70), (210, 210, 200)))
epub("afternoon.epub", "An Afternoon Light", "Élodie Carré", "Lanterne", 2004, "en", EN)
epub("small-boats.epub", "Small Boats", "Tobias Zimmer", "Lanterne", 1987, "en", EN, ((150, 90, 30), (250, 230, 170)))
pdf("counting-pages.pdf", "Counting Pages", "Ruth Hallam", 12)
print("wrote", len(list(out.iterdir())), "books to", out)
