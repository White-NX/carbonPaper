# -*- coding: utf-8 -*-
import sys, os, unittest
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", ".."))

from monitor.monitor.presidio_zh_recognizers import (
    ChineseIdCardRecognizer, ChinesePhoneRecognizer,
    ChineseBankCardRecognizer, ChineseNameRecognizer,
    ChineseAddressRecognizer, _validate_id_checksum,
    _validate_id_15, _validate_luhn, get_zh_recognizers,
)
from monitor.monitor.presidio_service import normalize_ocr_text, _remap_entities

_W = [7,9,10,5,8,4,2,1,6,3,7,9,10,5,8,4,2]
def _mk18(p): return p + "10X98765432"[sum(int(p[i])*_W[i] for i in range(17))%11]

def _pm(rec, text):
    ents = [rec.supported_entities[0]] if hasattr(rec,"supported_entities") else [rec.supported_entity]
    return [(r.start,r.end,r.score,text[r.start:r.end]) for r in rec.analyze(text=text,entities=ents)]


class TestIdCardValidation(unittest.TestCase):
    def test_18_valid(self):
        self.assertTrue(_validate_id_checksum(_mk18('11010119900307653')))
    def test_18_invalid(self):
        self.assertFalse(_validate_id_checksum('110101199003076530'))
    def test_15_valid(self):
        self.assertTrue(_validate_id_15('320112750915342'))
    def test_15_bad_month(self):
        self.assertFalse(_validate_id_15('320112751315342'))
    def test_15_bad_day(self):
        self.assertFalse(_validate_id_15('320112750932342'))

class TestIdCardRecognizer(unittest.TestCase):
    def setUp(self):
        self.rec = ChineseIdCardRecognizer()
    def test_18_in_chinese(self):
        fid = _mk18('11010119900307653')
        text = '身份证号' + fid + '，请核实'
        m = _pm(self.rec, text)
        self.assertEqual(len(m), 1, f'got {m}')
        self.assertEqual(m[0][3], fid)
    def test_15_in_chinese(self):
        text = '身份证号320112750915342，用户名谭辉'
        m = _pm(self.rec, text)
        self.assertEqual(len(m), 1, f'got {m}')
        self.assertEqual(m[0][3], '320112750915342')
    def test_no_false_short(self):
        text = '订单号123456'
        self.assertEqual(len(_pm(self.rec, text)), 0)

class TestPhoneRecognizer(unittest.TestCase):
    def setUp(self):
        self.rec = ChinesePhoneRecognizer()
    def test_adjacent_chinese(self):
        text = '电话号码是14728087589身份证号'
        m = _pm(self.rec, text)
        phones = [x[3] for x in m if len(x[3])==11]
        self.assertIn('14728087589', phones, f'got {m}')
    def test_standard(self):
        text = '请拨打13800138000联系'
        m = _pm(self.rec, text)
        phones = [x[3] for x in m if len(x[3])==11]
        self.assertIn('13800138000', phones)
    def test_no_false_short(self):
        text = '共12345个结果'
        mob = [x for x in _pm(self.rec,text) if len(x[3])==11]
        self.assertEqual(len(mob), 0)
    def test_at_start(self):
        text = '14728087589是电话'
        phones = [x[3] for x in _pm(self.rec,text) if len(x[3])==11]
        self.assertIn('14728087589', phones)
    def test_at_end(self):
        text = '电话14728087589'
        phones = [x[3] for x in _pm(self.rec,text) if len(x[3])==11]
        self.assertIn('14728087589', phones)

class TestLuhnValidation(unittest.TestCase):
    def test_valid(self):
        self.assertTrue(_validate_luhn('4111111111111111'))
    def test_invalid(self):
        self.assertFalse(_validate_luhn('4111111111111112'))
    def test_short(self):
        self.assertFalse(_validate_luhn('411111'))

class TestBankCardRecognizer(unittest.TestCase):
    def setUp(self):
        self.rec = ChineseBankCardRecognizer()
    def test_in_chinese(self):
        text = '银行卡号4111111111111111请确认'
        cards = [x[3] for x in _pm(self.rec, text)]
        self.assertIn('4111111111111111', cards)

class TestNameRecognizer(unittest.TestCase):
    def setUp(self):
        self.rec = ChineseNameRecognizer()
    def _ns(self, text):
        return [(r.start,r.end,r.score,text[r.start:r.end])
                for r in self.rec.analyze(text=text,entities=['PERSON'],nlp_artifacts=None)]
    def test_with_context(self):
        text = '用户名谭辉)已注册'
        names = [x[3] for x in self._ns(text) if x[2]>=0.3]
        self.assertIn('谭辉', names, f'got {names}')
    def test_crime_context(self):
        text = '犯罪嫌疑人劳荣枝使用化名为沈凌秋的身份证'
        names = [x[3] for x in self._ns(text) if x[2]>=0.3]
        self.assertIn('劳荣枝', names, f'got {names}')
        self.assertIn('沈凌秋', names, f'got {names}')
    def test_compound_surname(self):
        text = '被告人徐淼到庭'
        names = [x[3] for x in self._ns(text) if x[2]>=0.3]
        self.assertIn('徐淼', names, f'got {names}')
    def test_no_context_low_score(self):
        text = '今天天气不错'
        high = [x for x in self._ns(text) if x[2]>=0.3]
        self.assertEqual(len(high), 0, f'unexpected: {high}')

_SAMPLE = (
    '我是张凯文的爸爸王-良，sfz号码32011 2750915342，电话号码147 28087-589。'
    '1976年，撒贝宁出生成长于中国人民解放军海军南海舰队政治部机关大院，在湛江霞山区。父亲撒世贵，安徽和县人，是南海舰队政治部文工团的话剧演员，母亲邓雅娟是毕业于沈阳音乐学院的声乐演员[9]。妹妹撒贝娜，先前担任过舞蹈老师，现从事行政工作[10]。3岁时，撒贝宁进入南海舰队政治部幼儿园。撒贝宁8岁时父亲由军队转业去武汉市人民艺术剧院，举家迁武汉。'
)

class TestSampleText(unittest.TestCase):
    """End-to-end: OCR-noisy _SAMPLE through full PresidioService pipeline.

    This tests normalization + spaCy NER + custom recognizers together.
    With trf models, spaCy NER (Strategy 1) detects names that the
    surname heuristic alone (Strategy 2, nlp_artifacts=None) would miss.
    """
    @classmethod
    def setUpClass(cls):
        try:
            import spacy
            # Accept either trf or sm
            for model in ('zh_core_web_trf', 'zh_core_web_sm'):
                try:
                    spacy.load(model)
                    break
                except OSError:
                    continue
            else:
                raise unittest.SkipTest('no zh spaCy model installed')
        except ImportError:
            raise unittest.SkipTest('spacy not installed')
        from monitor.monitor.presidio_service import PresidioService
        cls.svc = PresidioService.get_instance()
        cls.svc.initialize('zh')

    def test_phone(self):
        res = self.svc.analyze([_SAMPLE])
        ents = res[0]
        # Normalization merges '147 28087-589' → detects 14728087589.
        # Remap gives offsets back into original _SAMPLE.
        phone_digits = {''.join(c for c in _SAMPLE[e['start']:e['end']] if c.isdigit())
                        for e in ents if e['entity_type'] == 'PHONE_NUMBER'}
        self.assertIn('14728087589', phone_digits, f'got {phone_digits}')

    def test_id_card(self):
        res = self.svc.analyze([_SAMPLE])
        ents = res[0]
        ids = {e['entity_type'] for e in ents}
        self.assertIn('CN_ID_CARD', ids, f'expected CN_ID_CARD, got {ents}')

    def test_names(self):
        res = self.svc.analyze([_SAMPLE])
        ents = res[0]
        names = {_SAMPLE[e['start']:e['end']] for e in ents if e['entity_type'] == 'PERSON'}
        self.assertIn('撒贝宁', names, f'got {names}')

class TestNormalization(unittest.TestCase):
    """Tests for OCR text normalisation (fullwidth digits, inter-digit space stripping)."""

    def test_fullwidth_digits(self):
        text = "\uff10\uff11\uff12\uff13\uff14\uff15\uff16\uff17\uff18\uff19"
        norm, mapping = normalize_ocr_text(text)
        self.assertEqual(norm, "0123456789")
        self.assertEqual(len(mapping), len(norm) + 1)

    def test_strip_spaces_between_digits(self):
        norm, _ = normalize_ocr_text("32011 2750915342")
        self.assertEqual(norm, "320112750915342")

    def test_strip_dashes_between_digits(self):
        norm, _ = normalize_ocr_text("147 28087-589")
        self.assertEqual(norm, "14728087589")

    def test_strip_multiple_chars_between_digits(self):
        norm, _ = normalize_ocr_text("123 - 456")
        self.assertEqual(norm, "123456")

    def test_preserve_non_digit_spaces(self):
        norm, _ = normalize_ocr_text("hello world 123")
        self.assertEqual(norm, "hello world 123")

    def test_preserve_leading_trailing_spaces(self):
        norm, _ = normalize_ocr_text(" 123 abc ")
        self.assertEqual(norm, " 123 abc ")

    def test_offset_mapping_identity(self):
        """Plain ASCII text → identity mapping."""
        text = "abc123"
        norm, mapping = normalize_ocr_text(text)
        self.assertEqual(norm, text)
        self.assertEqual(mapping, [0, 1, 2, 3, 4, 5, 6])

    def test_offset_mapping_with_strip(self):
        text = "1 2"
        norm, mapping = normalize_ocr_text(text)
        self.assertEqual(norm, "12")
        # norm[0] = '1' → orig[0], norm[1] = '2' → orig[2], sentinel → orig[3]
        self.assertEqual(mapping, [0, 2, 3])

    def test_remap_entities(self):
        norm_to_orig = [0, 2, 3, 5, 6, 7]  # sentinel at end
        entities = ({"entity_type": "TEST", "start": 1, "end": 4, "score": 0.9},)
        remapped = _remap_entities(entities, norm_to_orig)
        self.assertEqual(remapped[0]["start"], 2)
        self.assertEqual(remapped[0]["end"], 6)

    def test_ocr_noise_id_card(self):
        """OCR-noisy ID card number should be matched after normalisation."""
        rec = ChineseIdCardRecognizer()
        # Original noisy text as PaddleOCR might produce
        text = "sfz号码32011 2750915342"
        norm, mapping = normalize_ocr_text(text)
        # The normalised text should contain the clean ID
        self.assertIn("320112750915342", norm)
        # The recognizer should find it in normalised text
        m = _pm(rec, norm)
        self.assertEqual(len(m), 1, f"expected 1 match in normalised text, got {m}")

    def test_ocr_noise_phone(self):
        """OCR-noisy phone should be matched after normalisation."""
        rec = ChinesePhoneRecognizer()
        text = "电话号码147 28087-589"
        norm, _ = normalize_ocr_text(text)
        self.assertIn("14728087589", norm)
        phones = [x[3] for x in _pm(rec, norm) if len(x[3]) == 11]
        self.assertIn("14728087589", phones)


class TestPresidioIntegration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        try:
            import spacy; spacy.load('zh_core_web_trf')
        except Exception:
            raise unittest.SkipTest('zh_core_web_trf not installed')
        from monitor.monitor.presidio_service import PresidioService
        cls.svc = PresidioService.get_instance()
        cls.svc.initialize('zh')
    def test_full_pipeline(self):
        s = '劳荣枝的电话号码是14728087589身份证号320112750915342，用户名谭辉)。'
        res = self.svc.analyze([s])
        ents = res[0]
        types = {e['entity_type'] for e in ents}
        texts = {s[e['start']:e['end']] for e in ents}
        self.assertIn('PHONE_NUMBER', types)
        self.assertIn('14728087589', texts)
        self.assertIn('CN_ID_CARD', types)
    def test_batch(self):
        texts = ['张三的电话13900139000','李四的邮箱test@example.com','clean']
        res = self.svc.analyze(texts)
        self.assertEqual(len(res), 3)
        self.assertIn('PHONE_NUMBER', {e['entity_type'] for e in res[0]})
        self.assertEqual(len(res[2]), 0)

if __name__ == '__main__':
    unittest.main(verbosity=2)
