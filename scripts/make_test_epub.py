import zipfile, os, sys

OUT = sys.argv[1] if len(sys.argv) > 1 else "samples"
os.makedirs(OUT, exist_ok=True)

CHAPTERS = [
    ("初入山门", "他背着行囊站在山门前，抬头看那两株老松，风从谷底卷上来，带着湿冷的土腥气。"),
    ("夜谈", "灯芯爆了一下。老人把茶碗推过来，说：这条路走的人少，你要习惯一个人走。"),
    ("雪落", "雪下了三天。第四天早上，屋檐下的冰棱齐刷刷断了，像有人在外头敲了一记钟。"),
]


def chapter_xhtml(title, body):
    return f"""<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml"><head><title>{title}</title>
<style>p{{text-indent:2em}}</style></head>
<body><h2>{title}</h2><p>{body}</p><p>{body}</p></body></html>"""


def make_epub3(path):
    opf = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">test-epub3</dc:identifier>
    <dc:title>雪落山门</dc:title>
    <dc:creator>测试作者</dc:creator>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
""" + "".join(
        f'    <item id="c{i}" href="ch{i}.xhtml" media-type="application/xhtml+xml"/>\n'
        for i in range(len(CHAPTERS))
    ) + """  </manifest>
  <spine>
""" + "".join(f'    <itemref idref="c{i}"/>\n' for i in range(len(CHAPTERS))) + """  </spine>
</package>"""

    nav = """<?xml version="1.0" encoding="utf-8"?>
<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops">
<head><title>目录</title></head><body>
<nav epub:type="toc"><ol>
""" + "".join(
        f'<li><a href="ch{i}.xhtml">{t}</a></li>\n' for i, (t, _) in enumerate(CHAPTERS)
    ) + """</ol></nav></body></html>"""

    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml", """<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>""")
        z.writestr("OEBPS/content.opf", opf)
        z.writestr("OEBPS/nav.xhtml", nav)
        for i, (t, b) in enumerate(CHAPTERS):
            z.writestr(f"OEBPS/ch{i}.xhtml", chapter_xhtml(t, b))


def make_epub2(path):
    opf = """<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="2.0" unique-identifier="uid">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:identifier id="uid">test-epub2</dc:identifier>
    <dc:title>旧版 ncx 书</dc:title>
    <dc:language>zh-CN</dc:language>
  </metadata>
  <manifest>
    <item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/>
""" + "".join(
        f'    <item id="c{i}" href="text/c{i}.xhtml" media-type="application/xhtml+xml"/>\n'
        for i in range(len(CHAPTERS))
    ) + """  </manifest>
  <spine toc="ncx">
""" + "".join(f'    <itemref idref="c{i}"/>\n' for i in range(len(CHAPTERS))) + """  </spine>
</package>"""

    ncx = """<?xml version="1.0" encoding="utf-8"?>
<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/" version="2005-1">
<head><meta name="dtb:uid" content="test-epub2"/></head>
<docTitle><text>旧版 ncx 书</text></docTitle>
<navMap>
""" + "".join(
        f'<navPoint id="n{i}" playOrder="{i+1}"><navLabel><text>第{i+1}回 {t}</text></navLabel>'
        f'<content src="text/c{i}.xhtml"/></navPoint>\n'
        for i, (t, _) in enumerate(CHAPTERS)
    ) + """</navMap></ncx>"""

    with zipfile.ZipFile(path, "w") as z:
        z.writestr("mimetype", "application/epub+zip", compress_type=zipfile.ZIP_STORED)
        z.writestr("META-INF/container.xml", """<?xml version="1.0"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles>
</container>""")
        z.writestr("content.opf", opf)
        z.writestr("toc.ncx", ncx)
        for i, (t, b) in enumerate(CHAPTERS):
            z.writestr(f"text/c{i}.xhtml", chapter_xhtml(t, b))


make_epub3(os.path.join(OUT, "test_epub3.epub"))
make_epub2(os.path.join(OUT, "test_epub2.epub"))
print("ok")
