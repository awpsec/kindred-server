"""Local converter checks: python3 deploy/test-render-document.py.
Requires the same LibreOffice/system libraries as the server image.
"""
import importlib.util
import io
import os
from pathlib import Path
import socket
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location('renderer', Path(__file__).with_name('render-document.py'))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)


def document():
    data = io.BytesIO()
    with zipfile.ZipFile(data, 'w', zipfile.ZIP_DEFLATED) as z:
        z.writestr('[Content_Types].xml', '''<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>''')
        z.writestr('_rels/.rels', '''<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="r1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>''')
        text = '<w:p><w:r><w:t>A general report paragraph with text to flow across pages.</w:t></w:r></w:p>' * 150
        z.writestr('word/document.xml', '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>'+text+'<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:bottom="1440" w:left="1440" w:right="1440"/></w:sectPr></w:body></w:document>')
    return data.getvalue()


class RendererTests(unittest.TestCase):
    def test_invalid_and_expansion_limits(self):
        with self.assertRaises(zipfile.BadZipFile):
            r.validate(b'not a document')
        data = io.BytesIO()
        with zipfile.ZipFile(data, 'w', zipfile.ZIP_DEFLATED) as z:
            z.writestr('word/document.xml', b' ' * (65 * 1024 * 1024))
        with self.assertRaises(ValueError):
            r.validate(data.getvalue())

    def test_actual_layout(self):
        pdf = r.convert(document())
        self.assertTrue(pdf.startswith(b'%PDF-'))
        self.assertGreater(pdf.count(b'/Type/Page'), 2)

    def test_isolation(self):
        with tempfile.TemporaryDirectory() as allowed, tempfile.TemporaryDirectory() as other:
            secret = Path(other) / 'secret'
            secret.write_text('private fixture')
            pid = os.fork()
            if pid == 0:
                try:
                    r.restrict(allowed)
                    Path(allowed, 'ok').write_text('allowed')
                    for operation in [lambda: secret.read_text(), lambda: secret.write_text('bad'),
                                      lambda: socket.socket(socket.AF_INET, socket.SOCK_DGRAM),
                                      lambda: socket.socket(socket.AF_INET6, socket.SOCK_STREAM),
                                      lambda: socket.socket(socket.AF_UNIX),
                                      lambda: os.kill(os.getppid(), 0),
                                      lambda: os.fork()]:
                        try:
                            operation()
                        except PermissionError:
                            continue
                        os._exit(2)
                    os._exit(0)
                except BaseException:
                    os._exit(3)
            _, code = os.waitpid(pid, 0)
            self.assertEqual(code, 0)
            self.assertEqual(secret.read_text(), 'private fixture')


if __name__ == '__main__':
    unittest.main()
