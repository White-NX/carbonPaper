"""Custom Chinese PII recognizers for Microsoft Presidio.

Provides recognizers for:
- Chinese ID card numbers (18-digit with checksum, 15-digit legacy)
- Chinese mobile/landline phone numbers
- Chinese bank card numbers (Luhn validated)
- Chinese person names (via spaCy NER + common surname heuristic)
- Chinese addresses (via spaCy NER + regex)

**Important:** All regex patterns use ``(?<!\\d)`` / ``(?!\\d)`` instead of
``\\b`` for digit boundaries.  In Chinese text there are no spaces between
words, so ``\\b`` (which fires between ``\\w`` and ``\\W``) never triggers at
the boundary between a Chinese character and a digit — both are ``\\w``.
"""

import re
import logging

from presidio_analyzer import (
    Pattern,
    PatternRecognizer,
    EntityRecognizer,
    RecognizerResult,
    AnalysisExplanation,
)

logger = logging.getLogger(__name__)

# ──────────────────────────────────────────────────────────
# 1. Chinese ID Card Recognizer (18-digit + 15-digit legacy)
# ──────────────────────────────────────────────────────────

_ID_WEIGHTS = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
_ID_CHECK_CHARS = "10X98765432"


def _validate_id_checksum(id_str: str) -> bool:
    """Validate Chinese 18-digit ID card using ISO 7064 Mod 11-2."""
    if len(id_str) != 18:
        return False
    try:
        total = sum(int(id_str[i]) * _ID_WEIGHTS[i] for i in range(17))
        expected = _ID_CHECK_CHARS[total % 11]
        return id_str[17].upper() == expected
    except (ValueError, IndexError):
        return False


def _validate_id_15(id_str: str) -> bool:
    """Validate 15-digit old-format Chinese ID card (basic date check)."""
    if len(id_str) != 15 or not id_str.isdigit():
        return False
    try:
        month = int(id_str[8:10])
        day = int(id_str[10:12])
        return 1 <= month <= 12 and 1 <= day <= 31
    except (ValueError, IndexError):
        return False


class ChineseIdCardRecognizer(PatternRecognizer):
    """Recognizer for Chinese ID card numbers (18-digit and 15-digit)."""

    # Use (?<!\d) / (?!\d) instead of \b — see module docstring.
    PATTERNS = [
        Pattern(
            "cn_id_card_18",
            r"(?<!\d)[1-9]\d{5}(?:19|20)\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])\d{3}[\dXx](?!\d)",
            0.4,
        ),
        Pattern(
            "cn_id_card_15",
            r"(?<!\d)[1-9]\d{5}\d{2}(?:0[1-9]|1[0-2])(?:0[1-9]|[12]\d|3[01])\d{3}(?!\d)",
            0.35,
        ),
    ]
    CONTEXT = ["身份证", "证件号", "身份号码", "身份证号", "证件号码", "ID"]

    def __init__(self):
        super().__init__(
            supported_entity="CN_ID_CARD",
            patterns=self.PATTERNS,
            context=self.CONTEXT,
            supported_language="zh",
            name="ChineseIdCardRecognizer",
        )

    def validate_result(self, pattern_text: str) -> bool:
        text = pattern_text.strip()
        if len(text) == 18:
            return _validate_id_checksum(text)
        elif len(text) == 15:
            return _validate_id_15(text)
        return False


# ──────────────────────────────────────────────────────────
# 2. Chinese Phone Recognizer
# ──────────────────────────────────────────────────────────

class ChinesePhoneRecognizer(PatternRecognizer):
    """Recognizer for Chinese mobile and landline phone numbers."""

    # (?<!\d) / (?!\d) instead of \b for Chinese text compatibility.
    PATTERNS = [
        Pattern("cn_mobile", r"(?<!\d)1[3-9]\d{9}(?!\d)", 0.5),
        Pattern("cn_landline", r"(?<!\d)(?:0\d{2,3}-?)?\d{7,8}(?!\d)", 0.3),
    ]
    CONTEXT = ["手机", "电话", "联系方式", "联系电话", "手机号", "电话号码", "phone", "tel"]

    def __init__(self):
        super().__init__(
            supported_entity="PHONE_NUMBER",
            patterns=self.PATTERNS,
            context=self.CONTEXT,
            supported_language="zh",
            name="ChinesePhoneRecognizer",
        )


# ──────────────────────────────────────────────────────────
# 3. Chinese Bank Card Recognizer (Luhn check)
# ──────────────────────────────────────────────────────────

def _validate_luhn(number_str: str) -> bool:
    """Validate a number string using the Luhn algorithm."""
    try:
        digits = [int(d) for d in number_str]
    except ValueError:
        return False
    if len(digits) < 16 or len(digits) > 19:
        return False
    checksum = 0
    reverse = digits[::-1]
    for i, d in enumerate(reverse):
        if i % 2 == 1:
            d *= 2
            if d > 9:
                d -= 9
        checksum += d
    return checksum % 10 == 0


class ChineseBankCardRecognizer(PatternRecognizer):
    """Recognizer for Chinese bank card numbers (16-19 digits, Luhn)."""

    PATTERNS = [
        Pattern("cn_bank_card", r"(?<!\d)[3-6]\d{15,18}(?!\d)", 0.3),
    ]
    CONTEXT = ["银行卡", "卡号", "账号", "借记卡", "信用卡", "储蓄卡", "bank card"]

    def __init__(self):
        super().__init__(
            supported_entity="CN_BANK_CARD",
            patterns=self.PATTERNS,
            context=self.CONTEXT,
            supported_language="zh",
            name="ChineseBankCardRecognizer",
        )

    def validate_result(self, pattern_text: str) -> bool:
        return _validate_luhn(pattern_text.strip())


# ──────────────────────────────────────────────────────────
# 4. Chinese Name Recognizer (spaCy NER + common surname heuristic)
# ──────────────────────────────────────────────────────────

# Top ~120 Chinese surnames covering >85 % of population.
_COMMON_SURNAMES_2 = frozenset([
    "欧阳", "司马", "上官", "诸葛", "东方", "皇甫", "令狐", "慕容",
    "司徒", "公孙", "宇文", "长孙", "尉迟", "轩辕"
])
_COMMON_SURNAMES_1 = frozenset([
    "赵","钱","孙","李","周","吴","郑","王","冯","陈","褚","卫","蒋","沈","韩","杨","朱","秦","尤",
  "许","何","吕","施","张","孔","曹","严","华","金","魏","陶","姜","戚","谢","邹","喻","柏","水",
  "窦","章","云","苏","潘","葛","奚","范","彭","郎","鲁","韦","昌","马","苗","凤","花","方","俞",
  "任","袁","柳","酆","鲍","史","唐","费","廉","岑","薛","雷","贺","倪","汤","滕","殷","罗","毕",
  "郝","邬","安","常","乐","于","时","傅","皮","卞","齐","康","伍","余","元","卜","顾","孟","平",
  "黄","和","穆","萧","尹","姚","邵","湛","汪","祁","毛","禹","狄","米","贝","明","臧","计","伏",
  "成","戴","谈","宋","茅","庞","熊","纪","舒","屈","项","祝","董","梁","杜","阮","蓝","闵","席",
  "季","麻","强","贾","路","娄","危","江","童","颜","郭","梅","盛","林","刁","钟","徐","邱","骆",
  "高","夏","蔡","田","樊","胡","凌","霍","虞","万","支","柯","昝","管","卢","莫","经","房","裘",
  "缪","干","解","应","宗","丁","宣","贲","邓","郁","单","杭","洪","包","诸","左","石","崔","吉",
  "钮","龚","程","嵇","邢","滑","裴","陆","荣","翁","荀","羊","于","惠","甄","曲","家","封","芮",
  "羿","储","靳","汲","邴","糜","松","井","段","富","巫","乌","焦","巴","弓","牧","隗","山","谷",
  "车","侯","宓","蓬","全","郗","班","仰","秋","仲","伊","宫","宁","仇","栾","暴","甘","钭","厉",
  "戎","祖","武","符","刘","景","詹","束","龙","叶","幸","司","韶","郜","黎","蓟","溥","印","宿",
  "白","怀","蒲","邰","从","鄂","索","咸","籍","赖","卓","蔺","屠","蒙","池","乔","阴","郁","胥",
  "能","苍","双","闻","莘","党","翟","谭","贡","劳","逄","姬","申","扶","堵","冉","宰","郦","雍",
  "却","璩","桑","桂","濮","牛","寿","通","边","扈","燕","冀","浦","尚","农","温","别","庄","晏",
  "柴","瞿","阎","充","慕","连","茹","习","宦","艾","鱼","容","向","古","易","慎","戈","廖","庾",
  "终","暨","居","衡","步","都","耿","满","弘","匡","国","文","寇","广","禄","阙","东","欧","殳",
  "沃","利","蔚","越","夔","隆","师","巩","厍","聂","晁","勾","敖","融","冷","訾","辛","阚","那",
  "简","饶","空","曾","毋","沙","乜","养","鞠","须","丰","巢","关","蒯","相","查","后","荆","红",
  "游","郏","竺","权","逯","盖","益","桓","公","仉","督","岳","帅","缑","亢","况","郈","有","琴",
  "归","海","晋","楚","闫","法","汝","鄢","涂","钦","商","牟","佘","佴","伯","赏","墨","哈","谯",
  "篁","年","爱","阳","佟","言","福","南","火","铁","迟","漆","官","冼","真","展","繁","檀","祭",
  "密","敬","揭","舜","楼","疏","冒","浑","挚","胶","随","高","皋","原","种","练","弥","仓","眭",
  "蹇","覃","阿","门","恽","来","綦","召","仪","风","介","巨","木","京","狐","郇","虎","枚","抗",
  "达","杞","苌","折","麦","庆","过","竹","端","鲜","皇","亓","老","是","秘","畅","邝","还","宾",
  "闾","辜","纵","侴"
])

# Pattern: surname + 1-2 Chinese characters (given name)
_CJK = r"[\u4e00-\u9fff]"

# Build a regex that matches any known surname at current position.
# Compound (2-char) surnames are tried first so they take priority.
_SURNAME_ALTS_2 = "|".join(sorted(_COMMON_SURNAMES_2, key=len, reverse=True))
_SURNAME_ALTS_1 = "|".join(sorted(_COMMON_SURNAMES_1, key=len, reverse=True))
_SURNAME_PATTERN = re.compile(
    rf"(?:{_SURNAME_ALTS_2})|(?:{_SURNAME_ALTS_1})"
)

def _is_cjk(ch):
    return '\u4e00' <= ch <= '\u9fff'

# Words that, when they appear near a match, boost the likelihood it's a name
_NAME_CONTEXT_WORDS = frozenset([
    "姓名", "名叫", "化名", "用户名", "被告人", "犯罪嫌疑人", "嫌疑人",
    "当事人", "受害人", "被害人", "证人", "原告", "被告",
    "先生", "女士", "同志", "老师", "同学", "经理", "主任",
])


class ChineseNameRecognizer(EntityRecognizer):
    """Recognizer for Chinese person names.

    Uses two complementary strategies:
    1. spaCy NER — finds PERSON entities from the NLP pipeline
    2. Surname heuristic — common surname + 1-2 CJK chars, boosted
       by nearby context words.  Lower base score to reduce false positives.
    """

    ENTITIES = ["PERSON"]

    def __init__(self):
        super().__init__(
            supported_entities=self.ENTITIES,
            supported_language="zh",
            name="ChineseNameRecognizer",
        )

    def load(self):
        pass  # no additional loading needed

    def analyze(self, text, entities, nlp_artifacts=None, regex_flags=None):
        results = []
        seen_spans = set()

        # Strategy 1: spaCy NER
        if nlp_artifacts and nlp_artifacts.entities:
            for ent in nlp_artifacts.entities:
                if ent.label_ in ("PER", "PERSON"):
                    span = (ent.start_char, ent.end_char)
                    seen_spans.add(span)
                    results.append(RecognizerResult(
                        entity_type="PERSON",
                        start=ent.start_char,
                        end=ent.end_char,
                        score=0.6,
                        analysis_explanation=AnalysisExplanation(
                            recognizer=self.__class__.__name__,
                            pattern_name="spacy_ner",
                            original_score=0.6,
                        ),
                    ))

        # Strategy 2: surname heuristic — scan for known surnames, then
        # emit BOTH 2-char and 3-char name candidates at each position.
        # This ensures names like "徐淼" are detected even when followed
        # by another CJK character (greedy {1,2} would only give "徐淼到").
        for m in _SURNAME_PATTERN.finditer(text):
            surname_end = m.end()
            # Try given-name lengths 1 and 2
            for given_len in (1, 2):
                name_end = surname_end + given_len
                if name_end > len(text):
                    continue
                # All characters in the given name must be CJK
                given = text[surname_end:name_end]
                if not all(_is_cjk(c) for c in given):
                    continue
                span = (m.start(), name_end)
                if span in seen_spans:
                    continue
                # Check context: is there a name-related word within 10 chars?
                ctx_start = max(0, m.start() - 10)
                ctx_end = min(len(text), name_end + 10)
                context_window = text[ctx_start:ctx_end]
                has_context = any(w in context_window for w in _NAME_CONTEXT_WORDS)
                score = 0.55 if has_context else 0.25
                if score >= 0.3:
                    seen_spans.add(span)
                    results.append(RecognizerResult(
                        entity_type="PERSON",
                        start=m.start(),
                        end=name_end,
                        score=score,
                        analysis_explanation=AnalysisExplanation(
                            recognizer=self.__class__.__name__,
                            pattern_name="surname_heuristic",
                            original_score=score,
                        ),
                    ))

        return results


# ──────────────────────────────────────────────────────────
# 5. Chinese Address Recognizer (spaCy NER + regex)
# ──────────────────────────────────────────────────────────

_ADDRESS_PATTERN = re.compile(
    r"[\u4e00-\u9fff]{2,}(?:省|自治区)"
    r"[\u4e00-\u9fff]{2,}(?:市|自治州|地区|盟)"
    r"[\u4e00-\u9fff]{2,}(?:区|县|旗|市)"
    r"(?:[\u4e00-\u9fff\d]+(?:路|街|巷|道|村|镇|号|弄|室|栋|单元|层|楼))*"
)


class ChineseAddressRecognizer(EntityRecognizer):
    """Recognizer for Chinese addresses via spaCy NER + pattern matching."""

    ENTITIES = ["ADDRESS"]

    def __init__(self):
        super().__init__(
            supported_entities=self.ENTITIES,
            supported_language="zh",
            name="ChineseAddressRecognizer",
        )

    def load(self):
        pass

    def analyze(self, text, entities, nlp_artifacts=None, regex_flags=None):
        results = []

        # spaCy NER: LOC / GPE entities
        if nlp_artifacts and nlp_artifacts.entities:
            for ent in nlp_artifacts.entities:
                if ent.label_ in ("LOC", "GPE"):
                    results.append(RecognizerResult(
                        entity_type="ADDRESS",
                        start=ent.start_char,
                        end=ent.end_char,
                        score=0.5,
                        analysis_explanation=AnalysisExplanation(
                            recognizer=self.__class__.__name__,
                            pattern_name="spacy_ner",
                            original_score=0.5,
                        ),
                    ))

        # Regex for structured addresses (省市区...)
        for m in _ADDRESS_PATTERN.finditer(text):
            results.append(RecognizerResult(
                entity_type="ADDRESS",
                start=m.start(),
                end=m.end(),
                score=0.7,
                analysis_explanation=AnalysisExplanation(
                    recognizer=self.__class__.__name__,
                    pattern_name="address_regex",
                    original_score=0.7,
                ),
            ))

        return results


# ──────────────────────────────────────────────────────────
# Helper: get all custom Chinese recognizers
# ──────────────────────────────────────────────────────────

def get_zh_recognizers():
    """Return a list of all custom Chinese PII recognizers."""
    return [
        ChineseIdCardRecognizer(),
        ChinesePhoneRecognizer(),
        ChineseBankCardRecognizer(),
        ChineseNameRecognizer(),
        ChineseAddressRecognizer(),
    ]
