use super::*;
use serde::Deserialize;

fn all_kinds() -> PiiSettings {
    PiiSettings::new(PiiKind::SELECTABLE, false)
}

fn only(kind: PiiKind) -> PiiSettings {
    PiiSettings::new([kind], false)
}

fn kinds(text: &str, settings: PiiSettings) -> Vec<PiiKind> {
    detect(text, settings, Context::default())
        .into_iter()
        .map(|span| span.kind)
        .collect()
}

fn found(text: &str, settings: PiiSettings) -> Vec<(PiiKind, &str)> {
    detect(text, settings, Context::default())
        .into_iter()
        .map(|span| (span.kind, &text[span.start..span.end]))
        .collect()
}

fn id_number(first17: &str) -> String {
    let check = numbers_check_char(first17.as_bytes());
    format!("{first17}{}", check as char)
}

fn numbers_check_char(first17: &[u8]) -> u8 {
    const WEIGHTS: [u32; 17] = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2];
    let sum: u32 = first17
        .iter()
        .zip(WEIGHTS)
        .map(|(d, w)| u32::from(d - b'0') * w)
        .sum();
    b"10X98765432"[(sum % 11) as usize]
}

fn luhn_complete(body: &str) -> String {
    for d in b'0'..=b'9' {
        let candidate = format!("{body}{}", d as char);
        let sum: u32 = candidate
            .bytes()
            .rev()
            .enumerate()
            .map(|(i, b)| {
                let v = u32::from(b - b'0');
                if i % 2 == 1 {
                    let doubled = v * 2;
                    if doubled > 9 {
                        doubled - 9
                    } else {
                        doubled
                    }
                } else {
                    v
                }
            })
            .sum();
        if sum % 10 == 0 {
            return candidate;
        }
    }
    unreachable!()
}

#[test]
fn finds_mobile_numbers_with_and_without_labels() {
    for text in [
        "手机号：13812345678",
        "手机：138 1234 5678",
        "电话 138-1234-5678",
        "王小明  152 1234 1234",
        "请拨打13800138000联系",
        "１３８１２３４５６７８",
    ] {
        assert_eq!(kinds(text, all_kinds()), [PiiKind::PhoneNumber], "{text}");
    }
    assert_eq!(
        found("+86 138 1234 5678", all_kinds()),
        [(PiiKind::PhoneNumber, "+86 138 1234 5678")]
    );
    assert_eq!(
        found("Call me at (415) 555-0132", all_kinds()),
        [(PiiKind::PhoneNumber, "(415) 555-0132")]
    );
}

#[test]
fn landlines_need_an_area_code_or_a_label() {
    assert_eq!(
        kinds("Tel: 010-62345678", all_kinds()),
        [PiiKind::PhoneNumber]
    );
    assert_eq!(kinds("0571-88886666", all_kinds()), [PiiKind::PhoneNumber]);
    assert_eq!(kinds("电话 62345678", all_kinds()), [PiiKind::PhoneNumber]);
    for text in [
        "阅读 3456789 · 点赞 12034",
        "Build 20240923.1 released",
        "工号 20180315",
        "文件大小 1 048 576 字节",
        "会议号 123 456 789",
        "Intel Core 1234567890",
    ] {
        assert!(kinds(text, all_kinds()).is_empty(), "{text}");
    }
}

#[test]
fn id_numbers_survive_one_ocr_slip() {
    let id = id_number("33010619850712341");
    assert_eq!(kinds(&id, all_kinds()), [PiiKind::CnIdCard]);
    assert_eq!(
        kinds(&format!("身份证号：{id}"), all_kinds()),
        [PiiKind::CnIdCard]
    );

    // A swapped digit breaks the checksum but keeps province and birth date.
    let swapped = format!("{}8{}", &id[..15], &id[16..]);
    assert_eq!(kinds(&swapped, all_kinds()), [PiiKind::CnIdCard]);

    // CTC merges repeated digits, so "1111" in a birth date can come out as "111".
    let repeated = id_number("11010119901111652");
    let dropped = format!("{}{}", &repeated[..11], &repeated[12..]);
    assert_eq!(dropped.len(), 17);
    assert_eq!(kinds(&dropped, all_kinds()), [PiiKind::CnIdCard]);

    // The check character read as a lower-case x or a K is still masked.
    let with_x = id_number("64462719550326922");
    assert!(with_x.ends_with('X'));
    for read in [with_x.replace('X', "x"), with_x.replace('X', "K")] {
        let spans = detect(&read, all_kinds(), Context::default());
        assert_eq!(spans.len(), 1, "{read}");
        assert_eq!((spans[0].start, spans[0].end), (0, read.len()), "{read}");
    }

    // With a label, grouping and two slips are tolerated.
    assert_eq!(
        kinds("证件号码 546806 19890528 2938", all_kinds()),
        [PiiKind::CnIdCard]
    );
    assert_eq!(
        kinds("身份证号：13557820030114974", all_kinds()),
        [PiiKind::CnIdCard]
    );
}

#[test]
fn random_long_numbers_are_not_read_as_ids_or_cards() {
    for text in [
        "订单编号：203496817759901245",
        "时间戳 1695456000000",
        "学号 2021110123",
        "快递单号 SF1234567890123",
    ] {
        assert!(kinds(text, all_kinds()).is_empty(), "{text}");
    }
}

#[test]
fn bank_cards_by_checksum_scheme_or_grouping() {
    let unionpay = luhn_complete("622202123456789012");
    assert_eq!(unionpay.len(), 19);
    assert_eq!(kinds(&unionpay, all_kinds()), [PiiKind::BankCard]);
    assert_eq!(
        kinds("Card: 4111 1111 1111 1111", all_kinds()),
        [PiiKind::BankCard]
    );

    // Grouped in fours, a broken check digit still reads as a card.
    let card = luhn_complete("622202123456789");
    let last = card.as_bytes()[15];
    let broken = format!(
        "{} {} {} {}{}",
        &card[..4],
        &card[4..8],
        &card[8..12],
        &card[12..15],
        char::from(b'0' + (last - b'0' + 1) % 10)
    );
    assert_eq!(kinds(&broken, all_kinds()), [PiiKind::BankCard]);

    // A label accepts a card with a dropped digit and a merged group.
    assert_eq!(
        kinds("银行卡号：6222 8655645 9204 789", all_kinds()),
        [PiiKind::BankCard]
    );
}

#[test]
fn dates_times_versions_and_amounts_are_left_alone() {
    for text in [
        "更新于 2024-09-23 14:30:05",
        "2024年9月23日",
        "版本 3.12.10 (build 20240923.1)",
        "¥1,299.00 已优惠 ¥200.00",
        "Python 3.12.10 (tags/v3.12.10)",
        "第 1523 行，第 17 列",
        "Commit 4dedd7f on 2024/09/23",
        "共 12,345,678 条结果，用时 0.42 秒",
    ] {
        assert!(kinds(text, all_kinds()).is_empty(), "{text}");
    }
}

#[test]
fn ocr_separator_misreads_still_join_groups() {
    for text in [
        "手机： 181.4702 0889",
        "电话 183~4956-5317",
        "155 5151.9886",
    ] {
        assert_eq!(kinds(text, all_kinds()), [PiiKind::PhoneNumber], "{text}");
    }
    // Confusable letters inside a digit group are mapped back.
    assert_eq!(
        kinds("手机 l52 12S4 1234", all_kinds()),
        [PiiKind::PhoneNumber]
    );
}

#[test]
fn emails_skip_asset_names() {
    assert_eq!(
        found("邮箱：zhangsan@163.com", all_kinds()),
        [(PiiKind::EmailAddress, "zhangsan@163.com")]
    );
    assert!(kinds("icon@2x.png logo@3x.webp", all_kinds()).is_empty());
    // A phone number used as the local part is one e-mail address.
    assert_eq!(
        kinds("13812345678@qq.com", all_kinds()),
        [PiiKind::EmailAddress]
    );
}

#[test]
fn credentials_hide_the_secret_part() {
    let token = format!("ghp_{}", "A1b2C3d4".repeat(5));
    assert_eq!(
        found(&format!("token {token} ok"), all_kinds()),
        [(PiiKind::Credential, token.as_str())]
    );
    assert_eq!(
        kinds(
            "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwx",
            all_kinds()
        ),
        [PiiKind::Credential]
    );
    assert_eq!(
        kinds("AKIAIOSFODNN7EXAMPLE", all_kinds()),
        [PiiKind::Credential]
    );
    assert_eq!(
        found("postgres://admin:s3cret-pass@db.local/app", all_kinds()),
        [(PiiKind::Credential, "s3cret-pass")]
    );
    assert_eq!(
        found("密码：Hunter2024!", all_kinds()),
        [(PiiKind::Credential, "Hunter2024!")]
    );
    assert!(kinds("密码：********", all_kinds()).is_empty());
    assert!(kinds("忘记密码？", all_kinds()).is_empty());
    assert!(kinds("task-management board", all_kinds()).is_empty());
}

#[test]
fn ip_addresses_only_when_selected() {
    let text = "Server IP: 192.168.1.23 端口 23816";
    assert!(kinds(text, all_kinds().without_ip()).is_empty());
    assert_eq!(
        found(text, only(PiiKind::IpAddress)),
        [(PiiKind::IpAddress, "192.168.1.23")]
    );
    assert_eq!(
        found("addr fe80::1ff:fe23:4567:890a", only(PiiKind::IpAddress)),
        [(PiiKind::IpAddress, "fe80::1ff:fe23:4567:890a")]
    );
    assert!(kinds("会议 12:30:45 开始", only(PiiKind::IpAddress)).is_empty());
}

#[test]
fn street_addresses_need_a_street_and_number() {
    for (text, expected) in [
        (
            "收货地址：浙江省杭州市西湖区文三路477号",
            "浙江省杭州市西湖区文三路477号",
        ),
        ("北京市海淀区中关村大街27号", "北京市海淀区中关村大街27号"),
        ("阳光小区3栋2单元501室", "阳光小区3栋2单元501室"),
    ] {
        assert_eq!(
            found(text, only(PiiKind::Address)),
            [(PiiKind::Address, expected)]
        );
    }
    for text in [
        "浙江省杭州市",
        "地铁2号线",
        "北京 晴 25°C 空气质量良",
        "中美贸易谈判在日内瓦举行",
    ] {
        assert!(kinds(text, only(PiiKind::Address)).is_empty(), "{text}");
    }
}

#[test]
fn names_are_not_detected() {
    for text in [
        "张伟老师您好，作业已提交",
        "Meeting notes from Sarah Johnson",
        "同学们，明天的作业是第三章",
    ] {
        assert!(kinds(text, all_kinds()).is_empty(), "{text}");
    }
}

#[test]
fn long_numbers_are_masked_only_when_enabled() {
    let text = "订单编号：203496817759901245";
    assert!(kinds(text, all_kinds()).is_empty());
    let settings = PiiSettings::new(PiiKind::SELECTABLE, true);
    let spans = detect(text, settings, Context::default());
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0].kind, PiiKind::LongNumber);
    assert!(!spans[0].is_confident());
}

#[test]
fn labels_in_neighbouring_blocks_count() {
    let noisy = "13557820030114974";
    let row = |left: f64, right: f64, top: f64| {
        Some(Bounds {
            left,
            top,
            right,
            bottom: top + 20.0,
        })
    };
    // Label to the left on the same line.
    let blocks = [
        ("身份证号", row(10.0, 90.0, 100.0)),
        (noisy, row(300.0, 520.0, 101.0)),
    ];
    let contexts = block_contexts(&blocks);
    assert_eq!(contexts[1], Context::of("身份证号"));
    // Label directly above.
    let blocks = [
        ("身份证号", row(10.0, 90.0, 100.0)),
        (noisy, row(12.0, 230.0, 126.0)),
    ];
    assert_eq!(block_contexts(&blocks)[1], Context::of("身份证号"));
    // Unrelated block far below.
    let blocks = [
        ("身份证号", row(10.0, 90.0, 100.0)),
        (noisy, row(12.0, 230.0, 400.0)),
    ];
    assert_eq!(block_contexts(&blocks)[1], Context::default());
}

#[test]
fn mask_replaces_whole_spans_in_multibyte_text() {
    let text = "张三 138 1234 5678 👍 zhangsan@163.com";
    let spans = detect(text, all_kinds(), Context::default());
    assert_eq!(mask(text, &spans), "张三 [PHONE_NUMBER] 👍 [EMAIL_ADDRESS]");
    assert_eq!(mask(text, &[]), text);
}

#[test]
fn settings_names_round_trip() {
    for kind in PiiKind::SELECTABLE {
        assert_eq!(PiiKind::from_name(kind.name()), Some(kind));
    }
    assert_eq!(PiiKind::from_name("CREDIT_CARD"), Some(PiiKind::BankCard));
    assert_eq!(PiiKind::from_name("PERSON"), None);
    assert!(PiiSettings::off().is_off());
    assert!(!PiiSettings::new([PiiKind::LongNumber], false).wants(PiiKind::LongNumber));
}

impl PiiSettings {
    fn without_ip(self) -> Self {
        Self {
            kinds: self.kinds & !PiiKind::IpAddress.bit(),
            ..self
        }
    }
}

// ───────────── replay of observed OCR errors on freshly generated numbers ─────────────

/// xorshift64*, so the generated numbers are the same on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    fn digits(&mut self, n: usize) -> String {
        (0..n)
            .map(|_| char::from(b'0' + self.below(10) as u8))
            .collect()
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len() as u64) as usize]
    }

    fn phone(&mut self) -> String {
        format!(
            "1{}{}",
            self.pick(&["3", "4", "5", "6", "7", "8", "9"]),
            self.digits(9)
        )
    }

    fn id(&mut self) -> String {
        const PROVINCES: [&str; 31] = [
            "11", "12", "13", "14", "15", "21", "22", "23", "31", "32", "33", "34", "35", "36",
            "37", "41", "42", "43", "44", "45", "46", "50", "51", "52", "53", "54", "61", "62",
            "63", "64", "65",
        ];
        let first17 = format!(
            "{}{:02}{:02}{}{:02}{:02}{}",
            self.pick(&PROVINCES),
            1 + self.below(99),
            1 + self.below(99),
            1955 + self.below(51),
            1 + self.below(12),
            1 + self.below(28),
            self.digits(3)
        );
        id_number(&first17)
    }

    fn card(&mut self, len: usize) -> String {
        let prefix = if len == 19 {
            self.pick(&["6222", "6217", "6228"])
        } else {
            self.pick(&["62", "4", "51", "52", "53", "54", "55"])
        };
        let body = format!("{prefix}{}", self.digits(len - prefix.len() - 1));
        luhn_complete(&body)
    }
}

#[derive(Deserialize)]
struct Fixture {
    patterns: Vec<Pattern>,
}

#[derive(Deserialize)]
struct Pattern {
    kind: String,
    context: bool,
    layout: String,
    groups: Vec<usize>,
    sep: String,
    prefix: String,
    ops: Vec<Vec<serde_json::Value>>,
    digit_edits: usize,
}

impl Pattern {
    fn number(&self, rng: &mut Rng) -> String {
        let total: usize = self.groups.iter().sum();
        match self.kind.as_str() {
            "phone" => rng.phone(),
            "id" => rng.id(),
            _ => rng.card(total),
        }
    }

    fn shown(&self, number: &str) -> String {
        let mut parts = Vec::new();
        let mut at = 0;
        for len in &self.groups {
            parts.push(&number[at..at + len]);
            at += len;
        }
        parts.join(&self.sep)
    }

    /// Replays the recorded edits. Positions refer to the shown value.
    fn corrupt(&self, shown: &str) -> String {
        let mut chars: Vec<char> = shown.chars().collect();
        let mut ops: Vec<(usize, u8, usize, &serde_json::Value)> = self
            .ops
            .iter()
            .enumerate()
            .map(|(order, op)| {
                let position = op[1].as_u64().unwrap() as usize;
                let rank = if op[0] == "ins" { 1 } else { 0 };
                (position, rank, order, &op[0])
            })
            .collect();
        ops.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2)));
        for (position, _, order, name) in ops {
            let op = &self.ops[order];
            let ch = || op[2].as_str().unwrap().chars().next().unwrap();
            match name.as_str().unwrap() {
                "sub" => chars[position] = ch(),
                "del" => {
                    chars.remove(position);
                }
                _ => chars.insert(position, ch()),
            }
        }
        chars.into_iter().collect()
    }
}

struct Replay {
    confident: bool,
    visible_digits: usize,
}

fn replay(pattern: &Pattern, value: &str, settings: PiiSettings) -> Replay {
    let (text, context) = if pattern.layout == "form" {
        let blocks = [
            (
                pattern.prefix.as_str(),
                Some(Bounds {
                    left: 10.0,
                    top: 100.0,
                    right: 90.0,
                    bottom: 118.0,
                }),
            ),
            (
                value,
                Some(Bounds {
                    left: 220.0,
                    top: 100.0,
                    right: 480.0,
                    bottom: 118.0,
                }),
            ),
        ];
        (value.to_string(), block_contexts(&blocks)[1])
    } else {
        (format!("{}{value}", pattern.prefix), Context::default())
    };
    let spans = detect(&text, settings, context);
    let masked = mask(&text, &spans);
    // Digits OCR invented inside a label ("王小40") are not part of the number.
    let label_digits = if pattern.layout == "form" {
        0
    } else {
        pattern.prefix.chars().filter(char::is_ascii_digit).count()
    };
    let visible = masked.chars().filter(char::is_ascii_digit).count();
    Replay {
        confident: spans.iter().any(PiiSpan::is_confident),
        visible_digits: visible.saturating_sub(label_digits),
    }
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/ocr_error_patterns.json"))
        .expect("OCR error pattern fixture must parse")
}

#[test]
fn replayed_ocr_errors_are_detected_and_fully_masked() {
    const PER_PATTERN: usize = 12;
    let fixture = fixture();
    let settings = all_kinds();
    let mut rng = Rng(0x5eed_2026_0924);
    // (patterns tried, detected, masked with at most one digit left) per class
    let mut tally = std::collections::BTreeMap::<&str, (usize, usize, usize)>::new();
    for pattern in &fixture.patterns {
        let class = match (pattern.digit_edits, pattern.context) {
            (0, _) => "clean",
            (1, _) => "one slip",
            (2, true) => "two slips, labelled",
            _ => "heavier",
        };
        for _ in 0..PER_PATTERN {
            let number = pattern.number(&mut rng);
            let value = pattern.corrupt(&pattern.shown(&number));
            let result = replay(pattern, &value, settings);
            let entry = tally.entry(class).or_default();
            entry.0 += 1;
            entry.1 += usize::from(result.confident);
            entry.2 += usize::from(result.confident && result.visible_digits <= 1);
        }
    }
    let rate = |class: &str| {
        let (tried, detected, masked) = tally[class];
        (detected as f64 / tried as f64, masked as f64 / tried as f64)
    };
    for class in tally.keys() {
        let (detected, masked) = rate(class);
        eprintln!("{class:<22} detected {detected:.3} fully masked {masked:.3}");
    }
    // Measured when the rules were written: clean 1.000/1.000, one slip
    // 0.947/0.947, two labelled slips 0.907/0.885. Heavier damage is not
    // asserted; half the digits are gone and a reader cannot rebuild them either.
    let (clean, clean_masked) = rate("clean");
    assert_eq!(clean, 1.0, "separator-only changes must always be caught");
    assert_eq!(clean_masked, 1.0);
    let (one, one_masked) = rate("one slip");
    assert!(
        one >= 0.93 && one_masked >= 0.93,
        "one slip: {one:.3}/{one_masked:.3}"
    );
    let (two, two_masked) = rate("two slips, labelled");
    assert!(
        two >= 0.88 && two_masked >= 0.86,
        "two labelled slips: {two:.3}/{two_masked:.3}"
    );
}

#[test]
fn generated_screen_text_rarely_trips_the_rules() {
    let mut rng = Rng(0xfeed_2026_0924);
    // IP addresses are off by default; with them on, the "ip" line is a hit.
    let settings = all_kinds().without_ip();
    let mut hits = std::collections::BTreeMap::<&str, usize>::new();
    const ROUNDS: usize = 400;
    for _ in 0..ROUNDS {
        let year = 2019 + rng.below(8);
        let month = 1 + rng.below(12);
        let day = 1 + rng.below(28);
        let lines = [
            (
                "date",
                format!(
                    "更新于 {year}-{month:02}-{day:02} {:02}:{:02}:{:02}",
                    rng.below(24),
                    rng.below(60),
                    rng.below(60)
                ),
            ),
            (
                "order",
                format!("订单编号：{}{}", 1 + rng.below(9), rng.digits(17)),
            ),
            (
                "build",
                format!(
                    "版本 {}.{}.{} (build {year}{month:02}{day:02}.{})",
                    1 + rng.below(9),
                    rng.below(21),
                    rng.below(31),
                    1 + rng.below(9)
                ),
            ),
            (
                "size",
                format!("文件大小 {} 字节", 1_000_000 + rng.below(98_999_999)),
            ),
            (
                "views",
                format!(
                    "阅读 {}{} · 点赞 {}",
                    1 + rng.below(9),
                    rng.digits(6),
                    rng.digits(5)
                ),
            ),
            (
                "ip",
                format!(
                    "IP {}.{}.{}.{} 端口 {}",
                    10 + rng.below(214),
                    rng.below(256),
                    rng.below(256),
                    1 + rng.below(254),
                    1024 + rng.below(64511)
                ),
            ),
            (
                "timestamp",
                format!("时间戳 {}", 1_500_000_000_000 + rng.below(290_000_000_000)),
            ),
            (
                "meeting",
                format!(
                    "会议号 {}{} {} {}",
                    1 + rng.below(9),
                    rng.digits(2),
                    rng.digits(3),
                    rng.digits(3)
                ),
            ),
            (
                "staff",
                format!("工号 {}{}", 1 + rng.below(9), rng.digits(7)),
            ),
            (
                "student",
                format!("学号 {}{}", 1 + rng.below(9), rng.digits(9)),
            ),
            (
                "parcel",
                format!("快递单号 SF{}{}", 1 + rng.below(9), rng.digits(12)),
            ),
        ];
        for (name, line) in lines {
            if detect(&line, settings, Context::default())
                .iter()
                .any(PiiSpan::is_confident)
            {
                *hits.entry(name).or_default() += 1;
            }
        }
    }
    let total: usize = hits.values().sum();
    eprintln!("confident hits on ordinary lines: {hits:?}");
    for name in [
        "date",
        "build",
        "size",
        "views",
        "ip",
        "timestamp",
        "meeting",
        "staff",
        "student",
        "parcel",
    ] {
        assert_eq!(hits.get(name).copied().unwrap_or(0), 0, "{name}");
    }
    // Random 18-digit order numbers can pass the ID or card structure by chance.
    assert!(
        hits.get("order").copied().unwrap_or(0) <= ROUNDS / 100,
        "{hits:?}"
    );
    assert!(total <= ROUNDS / 100);
}
