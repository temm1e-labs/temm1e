//! Tiered Model Routing — classify tasks by complexity and route to
//! appropriate models. Simple tasks (file reads, status checks) use a
//! fast/cheap model; complex tasks (architecture, debugging) use the
//! most capable model. Classification is entirely rule-based (no LLM call).

use serde::{Deserialize, Serialize};
use temm1e_core::types::message::{ChatMessage, ContentPart, MessageContent, Role};
use temm1e_core::types::optimization::ExecutionProfile;
use tracing::{debug, info};

// ── Read-only / simple tool names ────────────────────────────────────────

/// Tools considered read-only / low-complexity. If a task uses only these
/// tools and meets other simplicity heuristics, it is classified as Simple.
const READ_ONLY_TOOLS: &[&str] = &[
    "file_read",
    "file_list",
    "check_messages",
    "git_status",
    "git_log",
    "git_diff",
    "http_get",
    "list_directory",
    "read_file",
];

/// Keywords in task descriptions that indicate a complex task.
/// Multilingual: EN, VI, ZH (Simplified + Traditional), JA, KO, ES, FR, PT, DE,
/// RU, AR, HI, TH, ID/MS, TR, PL, NL, IT, UK, SV
const COMPLEX_KEYWORDS: &[&str] = &[
    // English (exhaustive — formal, informal, abbreviations)
    "architecture",
    "architect",
    "debug",
    "debugging",
    "debugger",
    "refactor",
    "refactoring",
    "design",
    "redesign",
    "migrate",
    "migration",
    "optimize",
    "optimization",
    "optimise",
    "optimisation",
    "security audit",
    "performance",
    "investigate",
    "root cause",
    "rewrite",
    "rearchitect",
    "overhaul",
    "revamp",
    "deep dive",
    "postmortem",
    "post-mortem",
    "incident",
    "outage",
    "scalability",
    "scale",
    "bottleneck",
    "profiling",
    "benchmark",
    "load test",
    "stress test",
    "threat model",
    "vulnerability",
    "penetration test",
    "pentest",
    "code review",
    "tech debt",
    "technical debt",
    "system design",
    "capacity planning",
    "disaster recovery",
    "fault tolerance",
    "high availability",
    // Vietnamese (formal + informal + slang)
    "kiến trúc",
    "gỡ lỗi",
    "tái cấu trúc",
    "thiết kế",
    "thiết kế lại",
    "di chuyển",
    "di cư",
    "tối ưu",
    "tối ưu hóa",
    "kiểm tra bảo mật",
    "hiệu suất",
    "điều tra",
    "nguyên nhân gốc",
    "viết lại",
    "sửa lỗi",
    "phân tích lỗi",
    "xem xét mã",
    "nợ kỹ thuật",
    "kiểm thử tải",
    "kiểm thử áp lực",
    "khắc phục sự cố",
    "tìm lỗi",
    "phân tích hiệu suất",
    "cải thiện hiệu năng",
    "đánh giá bảo mật",
    "mô hình hóa mối đe dọa",
    "lỗ hổng",
    "xử lý sự cố",
    "khả năng mở rộng",
    "chịu lỗi",
    // Chinese (Simplified)
    "架构",
    "调试",
    "重构",
    "设计",
    "重新设计",
    "迁移",
    "优化",
    "安全审计",
    "性能",
    "调查",
    "根因",
    "根本原因",
    "重写",
    "代码审查",
    "技术债务",
    "压力测试",
    "负载测试",
    "性能分析",
    "瓶颈",
    "故障排除",
    "漏洞",
    "渗透测试",
    "容灾",
    "高可用",
    "扩展性",
    "可伸缩",
    // Chinese (Traditional)
    "架構",
    "調試",
    "重構",
    "設計",
    "遷移",
    "優化",
    "安全審計",
    "效能",
    "調查",
    "根因",
    "重寫",
    "程式碼審查",
    "技術債務",
    "壓力測試",
    "負載測試",
    "效能分析",
    "瓶頸",
    "故障排除",
    "漏洞",
    "滲透測試",
    // Japanese
    "アーキテクチャ",
    "デバッグ",
    "リファクタリング",
    "設計",
    "再設計",
    "移行",
    "最適化",
    "セキュリティ監査",
    "パフォーマンス",
    "調査",
    "根本原因",
    "書き直し",
    "コードレビュー",
    "技術的負債",
    "負荷テスト",
    "ストレステスト",
    "プロファイリング",
    "ボトルネック",
    "障害対応",
    "脆弱性",
    "ペネトレーションテスト",
    "可用性",
    "スケーラビリティ",
    // Korean
    "아키텍처",
    "디버그",
    "디버깅",
    "리팩토링",
    "설계",
    "재설계",
    "마이그레이션",
    "최적화",
    "보안 감사",
    "성능",
    "조사",
    "근본 원인",
    "재작성",
    "코드 리뷰",
    "기술 부채",
    "부하 테스트",
    "스트레스 테스트",
    "프로파일링",
    "병목",
    "장애 대응",
    "취약점",
    "확장성",
    "고가용성",
    // Spanish
    "arquitectura",
    "depurar",
    "depuración",
    "refactorizar",
    "diseño",
    "rediseño",
    "migrar",
    "migración",
    "optimizar",
    "optimización",
    "auditoría de seguridad",
    "rendimiento",
    "investigar",
    "causa raíz",
    "reescribir",
    "revisión de código",
    "deuda técnica",
    "prueba de carga",
    "prueba de estrés",
    "escalabilidad",
    "vulnerabilidad",
    "alta disponibilidad",
    // French
    "déboguer",
    "débogage",
    "refactoriser",
    "concevoir",
    "conception",
    "migrer",
    "optimiser",
    "audit de sécurité",
    "performance",
    "enquêter",
    "cause racine",
    "réécrire",
    "revue de code",
    "dette technique",
    "test de charge",
    "test de stress",
    "goulot d'étranglement",
    "vulnérabilité",
    "scalabilité",
    "haute disponibilité",
    // Portuguese
    "depurar",
    "depuração",
    "refatorar",
    "projetar",
    "migrar",
    "migração",
    "otimizar",
    "otimização",
    "auditoria de segurança",
    "desempenho",
    "investigar",
    "causa raiz",
    "reescrever",
    "revisão de código",
    "dívida técnica",
    "teste de carga",
    "teste de estresse",
    "gargalo",
    "vulnerabilidade",
    "escalabilidade",
    "alta disponibilidade",
    // German
    "debuggen",
    "refaktorieren",
    "entwerfen",
    "migrieren",
    "optimieren",
    "sicherheitsaudit",
    "leistung",
    "untersuchen",
    "grundursache",
    "umschreiben",
    "code-review",
    "technische schulden",
    "lasttest",
    "stresstest",
    "engpass",
    "schwachstelle",
    "skalierbarkeit",
    "hochverfügbarkeit",
    // Russian
    "архитектура",
    "отладка",
    "рефакторинг",
    "проектирование",
    "миграция",
    "оптимизация",
    "аудит безопасности",
    "производительность",
    "расследовать",
    "первопричина",
    "переписать",
    "ревью кода",
    "технический долг",
    "нагрузочное тестирование",
    "стресс-тест",
    "узкое место",
    "уязвимость",
    "масштабируемость",
    "отказоустойчивость",
    // Arabic
    "هندسة",
    "تصحيح",
    "إعادة هيكلة",
    "تصميم",
    "ترحيل",
    "تحسين",
    "تدقيق أمني",
    "أداء",
    "تحقيق",
    "السبب الجذري",
    "إعادة كتابة",
    "مراجعة الكود",
    "الديون التقنية",
    "اختبار الحمل",
    "اختبار الإجهاد",
    "عنق الزجاجة",
    "ثغرة أمنية",
    "قابلية التوسع",
    // Hindi
    "आर्किटेक्चर",
    "डीबग",
    "डिबगिंग",
    "रीफैक्टरिंग",
    "डिज़ाइन",
    "माइग्रेशन",
    "ऑप्टिमाइज़",
    "ऑप्टिमाइज़ेशन",
    "सुरक्षा ऑडिट",
    "प्रदर्शन",
    "जांच",
    "मूल कारण",
    "फिर से लिखें",
    "कोड रिव्यू",
    "तकनीकी ऋण",
    "लोड टेस्ट",
    "स्ट्रेस टेस्ट",
    "बॉटलनेक",
    "भेद्यता",
    "स्केलेबिलिटी",
    // Thai
    "สถาปัตยกรรม",
    "ดีบัก",
    "รีแฟกเตอร์",
    "ออกแบบ",
    "ย้ายระบบ",
    "เพิ่มประสิทธิภาพ",
    "ตรวจสอบความปลอดภัย",
    "ประสิทธิภาพ",
    "สืบสวน",
    "สาเหตุหลัก",
    "เขียนใหม่",
    "ทบทวนโค้ด",
    "หนี้เทคนิค",
    "ทดสอบโหลด",
    "คอขวด",
    "ช่องโหว่",
    // Indonesian / Malay
    "arsitektur",
    "debug",
    "refaktor",
    "desain",
    "migrasi",
    "optimasi",
    "audit keamanan",
    "performa",
    "investigasi",
    "akar masalah",
    "tulis ulang",
    "review kode",
    "utang teknis",
    "uji beban",
    "kerentanan",
    "skalabilitas",
    // Turkish
    "mimari",
    "hata ayıklama",
    "yeniden düzenleme",
    "tasarım",
    "göç",
    "optimizasyon",
    "güvenlik denetimi",
    "performans",
    "araştırma",
    "kök neden",
    "yeniden yazma",
    "kod inceleme",
    "teknik borç",
    "yük testi",
    "darboğaz",
    "güvenlik açığı",
    "ölçeklenebilirlik",
    // Polish
    "architektura",
    "debugowanie",
    "refaktoryzacja",
    "projektowanie",
    "migracja",
    "optymalizacja",
    "audyt bezpieczeństwa",
    "wydajność",
    "badanie",
    "przyczyna źródłowa",
    "przepisanie",
    "przegląd kodu",
    "dług techniczny",
    "test obciążenia",
    "wąskie gardło",
    "podatność",
    "skalowalność",
    // Dutch
    "architectuur",
    "debuggen",
    "refactoren",
    "ontwerpen",
    "migreren",
    "optimaliseren",
    "beveiligingsaudit",
    "prestatie",
    "onderzoeken",
    "grondoorzaak",
    "herschrijven",
    "code review",
    "technische schuld",
    "belastingstest",
    "knelpunt",
    "kwetsbaarheid",
    "schaalbaarheid",
    // Italian
    "architettura",
    "debug",
    "refactoring",
    "progettazione",
    "migrazione",
    "ottimizzazione",
    "audit di sicurezza",
    "prestazioni",
    "indagine",
    "causa principale",
    "riscrittura",
    "revisione del codice",
    "debito tecnico",
    "test di carico",
    "collo di bottiglia",
    "vulnerabilità",
    "scalabilità",
    // Ukrainian
    "архітектура",
    "налагодження",
    "рефакторинг",
    "проєктування",
    "міграція",
    "оптимізація",
    "аудит безпеки",
    "продуктивність",
    "розслідування",
    "першопричина",
    "переписати",
    "огляд коду",
    "технічний борг",
    "навантажувальне тестування",
    "вузьке місце",
    "вразливість",
    "масштабованість",
    // Swedish
    "arkitektur",
    "felsökning",
    "refaktorering",
    "design",
    "migrering",
    "optimering",
    "säkerhetsrevision",
    "prestanda",
    "undersöka",
    "grundorsak",
    "skriva om",
    "kodgranskning",
    "teknisk skuld",
    "belastningstest",
    "flaskhals",
    "sårbarhet",
    "skalbarhet",
];

/// Greeting/farewell patterns that indicate a trivial message.
/// Multilingual: EN, VI, ZH (Simplified + Traditional), JA, KO, ES, FR, PT, DE,
/// RU, AR, HI, TH, ID/MS, TR, PL, NL, IT, UK, SV
const TRIVIAL_PATTERNS: &[&str] = &[
    // English (exhaustive — formal, informal, slang, abbreviations)
    "hi",
    "hello",
    "hey",
    "yo",
    "sup",
    "howdy",
    "hiya",
    "heya",
    "thanks",
    "thank you",
    "thx",
    "ty",
    "tysm",
    "thank u",
    "cheers",
    "bye",
    "goodbye",
    "see ya",
    "later",
    "cya",
    "ttyl",
    "peace",
    "good morning",
    "good afternoon",
    "good evening",
    "good night",
    "gm",
    "gn",
    "ok",
    "okay",
    "k",
    "kk",
    "okie",
    "alright",
    "aight",
    "got it",
    "gotcha",
    "roger",
    "copy",
    "acknowledged",
    "noted",
    "sure",
    "yea",
    "yeah",
    "yes",
    "yep",
    "yup",
    "ya",
    "ye",
    "aye",
    "no",
    "nah",
    "nope",
    "naw",
    "cool",
    "nice",
    "great",
    "awesome",
    "perfect",
    "sweet",
    "dope",
    "sick",
    "amazing",
    "wonderful",
    "excellent",
    "brilliant",
    "fantastic",
    "lit",
    "understood",
    "makes sense",
    "fair enough",
    "i see",
    "ah ok",
    "oh ok",
    "np",
    "no problem",
    "no worries",
    "all good",
    "sounds good",
    "lgtm",
    "same",
    "true",
    "right",
    "correct",
    "exactly",
    "indeed",
    "agreed",
    "lol",
    "lmao",
    "haha",
    "hehe",
    "xd",
    // Emoji (universal)
    "\u{1f44d}",
    "\u{1f64f}",
    "\u{1f44c}",
    "\u{2764}",
    "\u{2705}",
    "\u{1f389}",
    "\u{1f60a}",
    "\u{1f642}",
    "\u{1f44f}",
    "\u{1f525}",
    "\u{2b50}",
    "\u{1f4af}",
    "\u{2714}",
    "\u{1f91d}",
    "\u{270c}",
    "\u{1f64c}",
    "\u{1f60d}",
    "\u{1f929}",
    // Vietnamese (formal + informal + regional + slang)
    "xin chào",
    "chào",
    "chào bạn",
    "chào anh",
    "chào chị",
    "chào em",
    "cảm ơn",
    "cám ơn",
    "cảm ơn bạn",
    "cảm ơn nhiều",
    "thanks",
    "tạm biệt",
    "bái bai",
    "bye",
    "chào buổi sáng",
    "chào buổi chiều",
    "chào buổi tối",
    "được",
    "ừ",
    "ờ",
    "ừm",
    "uh",
    "uhm",
    "rồi",
    "xong",
    "hiểu rồi",
    "hiểu",
    "ok rồi",
    "vâng",
    "dạ",
    "dạ vâng",
    "vâng ạ",
    "không",
    "ko",
    "k",
    "hông",
    "hem",
    "tốt",
    "hay",
    "tuyệt",
    "tuyệt vời",
    "xuất sắc",
    "giỏi",
    "đúng",
    "đúng rồi",
    "chính xác",
    "chuẩn",
    "ổn",
    "ok",
    "okie",
    "oke",
    "hay đấy",
    "ngon",
    "xịn",
    "max",
    // Chinese (Simplified)
    "你好",
    "嗨",
    "嘿",
    "哈喽",
    "早",
    "早安",
    "晚安",
    "谢谢",
    "感谢",
    "多谢",
    "谢了",
    "thx",
    "再见",
    "拜拜",
    "拜",
    "回见",
    "走了",
    "早上好",
    "下午好",
    "晚上好",
    "好的",
    "好",
    "行",
    "可以",
    "没问题",
    "嗯",
    "嗯嗯",
    "是",
    "是的",
    "对",
    "对的",
    "没错",
    "确实",
    "不是",
    "不",
    "不行",
    "不了",
    "明白",
    "了解",
    "知道了",
    "收到",
    "懂了",
    "棒",
    "厉害",
    "牛",
    "赞",
    "完美",
    "太好了",
    "不错",
    "很好",
    "哈哈",
    "呵呵",
    "嘻嘻",
    // Chinese (Traditional)
    "你好",
    "嗨",
    "謝謝",
    "感謝",
    "再見",
    "掰掰",
    "早安",
    "晚安",
    "好的",
    "行",
    "嗯",
    "是",
    "不是",
    "明白",
    "了解",
    "收到",
    "棒",
    "讚",
    "完美",
    "太好了",
    "不錯",
    // Japanese (formal + informal + casual)
    "こんにちは",
    "おはよう",
    "おはようございます",
    "こんばんは",
    "ありがとう",
    "ありがとうございます",
    "どうも",
    "サンキュー",
    "さようなら",
    "じゃあね",
    "またね",
    "バイバイ",
    "おやすみ",
    "はい",
    "うん",
    "ええ",
    "そうです",
    "いいえ",
    "いや",
    "ううん",
    "了解",
    "了解です",
    "わかった",
    "わかりました",
    "承知しました",
    "りょ",
    "いいね",
    "すごい",
    "素晴らしい",
    "完璧",
    "最高",
    "ナイス",
    "おけ",
    "おっけー",
    "オッケー",
    "笑",
    "草",
    "www",
    // Korean (formal + informal)
    "안녕",
    "안녕하세요",
    "안녕하십니까",
    "감사합니다",
    "감사해요",
    "고마워",
    "고맙습니다",
    "땡큐",
    "잘가",
    "안녕히 가세요",
    "바이바이",
    "좋은 아침",
    "좋은 저녁",
    "네",
    "예",
    "응",
    "어",
    "그래",
    "아니요",
    "아니",
    "아뇨",
    "알겠어",
    "알겠습니다",
    "이해했어",
    "ㅇㅋ",
    "ㅇㅇ",
    "좋아",
    "좋아요",
    "좋습니다",
    "멋져",
    "완벽",
    "최고",
    "대박",
    "ㅋㅋ",
    "ㅋㅋㅋ",
    "ㅎㅎ",
    "ㅎㅎㅎ",
    // Spanish
    "hola",
    "buenas",
    "qué tal",
    "hey",
    "gracias",
    "muchas gracias",
    "grax",
    "adiós",
    "chao",
    "nos vemos",
    "hasta luego",
    "buenos días",
    "buenas tardes",
    "buenas noches",
    "vale",
    "ok",
    "de acuerdo",
    "entendido",
    "listo",
    "sí",
    "si",
    "claro",
    "por supuesto",
    "no",
    "nada",
    "para nada",
    "genial",
    "perfecto",
    "excelente",
    "increíble",
    "guay",
    "mola",
    "jaja",
    "jajaja",
    // French
    "salut",
    "bonjour",
    "bonsoir",
    "coucou",
    "merci",
    "merci beaucoup",
    "au revoir",
    "à bientôt",
    "salut",
    "ciao",
    "oui",
    "ouais",
    "mouais",
    "non",
    "nan",
    "d'accord",
    "ok",
    "compris",
    "entendu",
    "c'est noté",
    "vu",
    "super",
    "parfait",
    "génial",
    "excellent",
    "magnifique",
    "top",
    "nickel",
    "mdr",
    "ptdr",
    // Portuguese
    "olá",
    "oi",
    "e aí",
    "fala",
    "obrigado",
    "obrigada",
    "valeu",
    "vlw",
    "tchau",
    "até mais",
    "falou",
    "flw",
    "bom dia",
    "boa tarde",
    "boa noite",
    "sim",
    "claro",
    "com certeza",
    "não",
    "nope",
    "tudo bem",
    "beleza",
    "show",
    "massa",
    "legal",
    "perfeito",
    "excelente",
    "entendi",
    "entendido",
    "kkk",
    "kkkk",
    "haha",
    "rsrs",
    // German
    "hallo",
    "hi",
    "moin",
    "servus",
    "grüß gott",
    "danke",
    "danke schön",
    "vielen dank",
    "tschüss",
    "auf wiedersehen",
    "ciao",
    "guten morgen",
    "guten tag",
    "guten abend",
    "gute nacht",
    "ja",
    "jo",
    "jep",
    "jawohl",
    "nein",
    "nö",
    "nee",
    "alles klar",
    "verstanden",
    "in ordnung",
    "geht klar",
    "passt",
    "toll",
    "super",
    "prima",
    "perfekt",
    "klasse",
    "spitze",
    "geil",
    // Russian
    "привет",
    "здравствуйте",
    "здравствуй",
    "хай",
    "хей",
    "йо",
    "спасибо",
    "спс",
    "благодарю",
    "пока",
    "до свидания",
    "до встречи",
    "бай",
    "доброе утро",
    "добрый день",
    "добрый вечер",
    "спокойной ночи",
    "да",
    "ага",
    "угу",
    "ок",
    "окей",
    "лады",
    "ладно",
    "нет",
    "не",
    "неа",
    "хорошо",
    "понял",
    "понятно",
    "ясно",
    "принято",
    "отлично",
    "круто",
    "класс",
    "супер",
    "зачёт",
    "кайф",
    "огонь",
    "лол",
    "ахах",
    "хаха",
    // Arabic
    "مرحبا",
    "أهلا",
    "أهلاً",
    "السلام عليكم",
    "هلا",
    "شكرا",
    "شكراً",
    "مشكور",
    "مع السلامة",
    "باي",
    "صباح الخير",
    "مساء الخير",
    "نعم",
    "أيوه",
    "اي",
    "ايوا",
    "لا",
    "لأ",
    "حسنا",
    "تمام",
    "ماشي",
    "أوكي",
    "ممتاز",
    "مفهوم",
    "واضح",
    "رائع",
    "حلو",
    "هههه",
    "ههه",
    // Hindi
    "नमस्ते",
    "नमस्कार",
    "हैलो",
    "हाय",
    "धन्यवाद",
    "शुक्रिया",
    "थैंक्स",
    "अलविदा",
    "बाय",
    "सुप्रभात",
    "शुभ संध्या",
    "शुभ रात्रि",
    "हाँ",
    "हां",
    "जी",
    "जी हाँ",
    "बिल्कुल",
    "नहीं",
    "ना",
    "ठीक है",
    "ठीक",
    "ओके",
    "अच्छा",
    "सही",
    "समझ गया",
    "समझा",
    "पता चला",
    "बहुत बढ़िया",
    "शानदार",
    "ज़बरदस्त",
    "मस्त",
    "क्लास",
    // Thai
    "สวัสดี",
    "สวัสดีครับ",
    "สวัสดีค่ะ",
    "หวัดดี",
    "ขอบคุณ",
    "ขอบคุณครับ",
    "ขอบคุณค่ะ",
    "แต๊งกิ้ว",
    "ลาก่อน",
    "บาย",
    "ไปก่อนนะ",
    "อรุณสวัสดิ์",
    "ราตรีสวัสดิ์",
    "ใช่",
    "ครับ",
    "ค่ะ",
    "จ้า",
    "จ้ะ",
    "ไม่",
    "เปล่า",
    "ไม่ใช่",
    "โอเค",
    "ตกลง",
    "เข้าใจ",
    "รับทราบ",
    "เยี่ยม",
    "สุดยอด",
    "เจ๋ง",
    "ดีมาก",
    "เริ่ด",
    "555",
    "5555",
    // Indonesian / Malay
    "halo",
    "hai",
    "hey",
    "terima kasih",
    "makasih",
    "thanks",
    "selamat tinggal",
    "dadah",
    "bye",
    "selamat pagi",
    "selamat malam",
    "ya",
    "iya",
    "yoi",
    "iye",
    "tidak",
    "nggak",
    "gak",
    "enggak",
    "oke",
    "ok",
    "siap",
    "paham",
    "mengerti",
    "bagus",
    "mantap",
    "keren",
    "sempurna",
    "luar biasa",
    "wkwk",
    "wkwkwk",
    "haha",
    // Turkish
    "merhaba",
    "selam",
    "hey",
    "teşekkürler",
    "teşekkür ederim",
    "sağol",
    "hoşça kal",
    "görüşürüz",
    "bay bay",
    "günaydın",
    "iyi akşamlar",
    "iyi geceler",
    "evet",
    "he",
    "hı",
    "hayır",
    "yok",
    "tamam",
    "ok",
    "anlaşıldı",
    "anladım",
    "harika",
    "mükemmel",
    "süper",
    "güzel",
    // Polish
    "cześć",
    "hej",
    "siema",
    "witam",
    "dzięki",
    "dziękuję",
    "pa",
    "do widzenia",
    "nara",
    "dzień dobry",
    "dobry wieczór",
    "dobranoc",
    "tak",
    "no",
    "ta",
    "nie",
    "ok",
    "okej",
    "rozumiem",
    "jasne",
    "spoko",
    "super",
    "świetnie",
    "idealnie",
    "ekstra",
    // Dutch
    "hallo",
    "hoi",
    "hey",
    "dag",
    "bedankt",
    "dankje",
    "dank je wel",
    "doei",
    "tot ziens",
    "dag",
    "goedemorgen",
    "goedenavond",
    "goedenacht",
    "ja",
    "jawel",
    "nee",
    "oké",
    "begrepen",
    "duidelijk",
    "prima",
    "gaaf",
    "top",
    "mooi",
    "perfect",
    // Italian
    "ciao",
    "salve",
    "buongiorno",
    "buonasera",
    "grazie",
    "grazie mille",
    "arrivederci",
    "addio",
    "buonanotte",
    "sì",
    "certo",
    "sicuro",
    "no",
    "ok",
    "va bene",
    "capito",
    "inteso",
    "perfetto",
    "fantastico",
    "ottimo",
    "bello",
    "grande",
    // Ukrainian
    "привіт",
    "здоровенькі були",
    "вітаю",
    "дякую",
    "дякую",
    "бувай",
    "до побачення",
    "доброго ранку",
    "добрий день",
    "добрий вечір",
    "так",
    "ага",
    "угу",
    "ні",
    "не",
    "добре",
    "зрозумів",
    "зрозуміло",
    "ок",
    "чудово",
    "круто",
    "клас",
    "супер",
    // Swedish
    "hej",
    "hallå",
    "tjena",
    "tja",
    "tack",
    "tack så mycket",
    "hejdå",
    "vi ses",
    "god morgon",
    "god kväll",
    "god natt",
    "ja",
    "japp",
    "jo",
    "nej",
    "nä",
    "okej",
    "fattar",
    "förstått",
    "klart",
    "bra",
    "toppen",
    "perfekt",
    "grym",
    "najs",
];

/// Action verbs that indicate a non-trivial task.
/// Multilingual: EN, VI, ZH (Simplified + Traditional), JA, KO, ES, FR, PT, DE,
/// RU, AR, HI, TH, ID/MS, TR, PL, NL, IT, UK, SV
const ACTION_VERBS: &[&str] = &[
    // English (exhaustive — formal, informal, imperative, polite)
    "find",
    "create",
    "run",
    "deploy",
    "read",
    "write",
    "search",
    "build",
    "fix",
    "update",
    "delete",
    "install",
    "configure",
    "setup",
    "set up",
    "check",
    "test",
    "compile",
    "execute",
    "fetch",
    "download",
    "upload",
    "send",
    "list",
    "show",
    "display",
    "open",
    "close",
    "start",
    "stop",
    "restart",
    "analyze",
    "analyse",
    "explain",
    "help me",
    "can you",
    "please",
    "could you",
    "would you",
    "generate",
    "convert",
    "transform",
    "compare",
    "merge",
    "split",
    "sort",
    "filter",
    "count",
    "calculate",
    "compute",
    "rename",
    "move",
    "copy",
    "paste",
    "undo",
    "redo",
    "connect",
    "disconnect",
    "sync",
    "backup",
    "restore",
    "reset",
    "enable",
    "disable",
    "toggle",
    "switch",
    "change",
    "modify",
    "edit",
    "add",
    "remove",
    "insert",
    "append",
    "prepend",
    "replace",
    "swap",
    "print",
    "log",
    "dump",
    "export",
    "import",
    "parse",
    "format",
    "validate",
    "verify",
    "confirm",
    "approve",
    "reject",
    "publish",
    "schedule",
    "cancel",
    "abort",
    "kill",
    "terminate",
    "clean",
    "clear",
    "purge",
    "flush",
    "trim",
    "truncate",
    "compress",
    "decompress",
    "encrypt",
    "decrypt",
    "sign",
    "hash",
    "encode",
    "decode",
    "monitor",
    "watch",
    "track",
    "trace",
    "profile",
    "measure",
    "look up",
    "look into",
    "figure out",
    "work on",
    "set up",
    "spin up",
    "tear down",
    "roll back",
    "scale up",
    "scale down",
    // Vietnamese (formal + informal + imperative)
    "tìm",
    "tạo",
    "chạy",
    "triển khai",
    "đọc",
    "viết",
    "tìm kiếm",
    "xây dựng",
    "xây",
    "sửa",
    "cập nhật",
    "xóa",
    "xoá",
    "cài đặt",
    "cấu hình",
    "thiết lập",
    "kiểm tra",
    "biên dịch",
    "thực thi",
    "tải về",
    "tải xuống",
    "tải lên",
    "gửi",
    "liệt kê",
    "hiển thị",
    "hiện",
    "mở",
    "đóng",
    "bắt đầu",
    "dừng",
    "dừng lại",
    "tắt",
    "khởi động",
    "khởi động lại",
    "phân tích",
    "giải thích",
    "giúp tôi",
    "giúp mình",
    "giúp em",
    "hãy",
    "cho tôi",
    "cho mình",
    "làm",
    "làm cho",
    "thêm",
    "bớt",
    "bỏ",
    "đổi",
    "đổi tên",
    "sao chép",
    "di chuyển",
    "nén",
    "giải nén",
    "mã hóa",
    "giải mã",
    "kết nối",
    "ngắt kết nối",
    "đồng bộ",
    "sao lưu",
    "khôi phục",
    "bật",
    "tắt",
    "chuyển đổi",
    "so sánh",
    "gộp",
    "tách",
    "đếm",
    "tính",
    "xuất",
    "nhập",
    "xác nhận",
    "xác minh",
    "theo dõi",
    "giám sát",
    "đo",
    "in",
    // Chinese (Simplified)
    "查找",
    "找",
    "创建",
    "建",
    "运行",
    "跑",
    "部署",
    "读取",
    "读",
    "写入",
    "写",
    "搜索",
    "搜",
    "构建",
    "修复",
    "修",
    "更新",
    "删除",
    "删",
    "安装",
    "装",
    "配置",
    "设置",
    "检查",
    "查",
    "测试",
    "编译",
    "执行",
    "下载",
    "上传",
    "发送",
    "发",
    "列出",
    "列",
    "显示",
    "看",
    "打开",
    "开",
    "关闭",
    "关",
    "启动",
    "停止",
    "停",
    "重启",
    "分析",
    "解释",
    "帮我",
    "请",
    "帮忙",
    "能不能",
    "可以",
    "添加",
    "加",
    "移除",
    "去掉",
    "复制",
    "拷贝",
    "移动",
    "重命名",
    "改名",
    "合并",
    "拆分",
    "排序",
    "过滤",
    "计算",
    "算",
    "统计",
    "转换",
    "格式化",
    "压缩",
    "解压",
    "加密",
    "解密",
    "连接",
    "断开",
    "同步",
    "备份",
    "恢复",
    "监控",
    "跟踪",
    "导出",
    "导入",
    // Chinese (Traditional)
    "查找",
    "建立",
    "執行",
    "部署",
    "讀取",
    "寫入",
    "搜尋",
    "建構",
    "修復",
    "更新",
    "刪除",
    "安裝",
    "設定",
    "檢查",
    "測試",
    "編譯",
    "下載",
    "上傳",
    "傳送",
    "顯示",
    "開啟",
    "關閉",
    "啟動",
    "停止",
    "重啟",
    "分析",
    "解釋",
    "幫我",
    "請",
    // Japanese
    "探す",
    "見つける",
    "作成",
    "作る",
    "実行",
    "走らせる",
    "デプロイ",
    "読む",
    "読み込む",
    "書く",
    "書き込む",
    "検索",
    "ビルド",
    "修正",
    "直す",
    "更新",
    "削除",
    "消す",
    "インストール",
    "入れる",
    "設定",
    "確認",
    "テスト",
    "コンパイル",
    "ダウンロード",
    "落とす",
    "アップロード",
    "上げる",
    "送信",
    "送る",
    "表示",
    "見せる",
    "開く",
    "閉じる",
    "起動",
    "立ち上げる",
    "停止",
    "止める",
    "再起動",
    "分析",
    "説明",
    "教えて",
    "して",
    "してください",
    "お願い",
    "追加",
    "削る",
    "移動",
    "コピー",
    "変更",
    "変える",
    "名前変更",
    "結合",
    "マージ",
    "分割",
    "ソート",
    "フィルター",
    "計算",
    "変換",
    "フォーマット",
    "圧縮",
    "解凍",
    "暗号化",
    "復号",
    "接続",
    "切断",
    "同期",
    "バックアップ",
    "復元",
    "監視",
    "エクスポート",
    "インポート",
    // Korean
    "찾기",
    "찾아",
    "만들기",
    "만들어",
    "실행",
    "돌려",
    "배포",
    "읽기",
    "읽어",
    "쓰기",
    "써",
    "검색",
    "빌드",
    "수정",
    "고쳐",
    "업데이트",
    "삭제",
    "지워",
    "설치",
    "깔아",
    "설정",
    "확인",
    "테스트",
    "컴파일",
    "다운로드",
    "받아",
    "업로드",
    "올려",
    "보내기",
    "보내",
    "목록",
    "표시",
    "보여줘",
    "열기",
    "열어",
    "닫기",
    "닫아",
    "시작",
    "중지",
    "멈춰",
    "재시작",
    "분석",
    "설명",
    "설명해줘",
    "도와줘",
    "해줘",
    "해주세요",
    "추가",
    "제거",
    "이동",
    "복사",
    "변경",
    "바꿔",
    "이름 변경",
    "병합",
    "분할",
    "정렬",
    "필터",
    "계산",
    "변환",
    "포맷",
    "압축",
    "암호화",
    "복호화",
    "연결",
    "동기화",
    "백업",
    "복원",
    "모니터링",
    "내보내기",
    "가져오기",
    // Spanish
    "buscar",
    "crear",
    "ejecutar",
    "correr",
    "desplegar",
    "leer",
    "escribir",
    "construir",
    "arreglar",
    "reparar",
    "actualizar",
    "eliminar",
    "borrar",
    "instalar",
    "configurar",
    "comprobar",
    "verificar",
    "probar",
    "compilar",
    "descargar",
    "subir",
    "cargar",
    "enviar",
    "mostrar",
    "enseñar",
    "abrir",
    "cerrar",
    "iniciar",
    "arrancar",
    "detener",
    "parar",
    "reiniciar",
    "analizar",
    "explicar",
    "ayúdame",
    "por favor",
    "puedes",
    "podrías",
    "añadir",
    "agregar",
    "quitar",
    "mover",
    "copiar",
    "renombrar",
    "combinar",
    "dividir",
    "ordenar",
    "filtrar",
    "calcular",
    "convertir",
    "formatear",
    "comprimir",
    "cifrar",
    "conectar",
    "sincronizar",
    "respaldar",
    "restaurar",
    "monitorear",
    "exportar",
    "importar",
    // French
    "chercher",
    "trouver",
    "créer",
    "exécuter",
    "lancer",
    "déployer",
    "lire",
    "écrire",
    "construire",
    "corriger",
    "réparer",
    "mettre à jour",
    "supprimer",
    "effacer",
    "installer",
    "configurer",
    "vérifier",
    "tester",
    "compiler",
    "télécharger",
    "envoyer",
    "afficher",
    "montrer",
    "ouvrir",
    "fermer",
    "démarrer",
    "arrêter",
    "redémarrer",
    "analyser",
    "expliquer",
    "aidez-moi",
    "aide-moi",
    "s'il vous plaît",
    "s'il te plaît",
    "peux-tu",
    "pouvez-vous",
    "ajouter",
    "retirer",
    "déplacer",
    "copier",
    "renommer",
    "fusionner",
    "diviser",
    "trier",
    "filtrer",
    "calculer",
    "convertir",
    "formater",
    "compresser",
    "chiffrer",
    "connecter",
    "synchroniser",
    "sauvegarder",
    "restaurer",
    "surveiller",
    "exporter",
    "importer",
    // Portuguese
    "procurar",
    "encontrar",
    "criar",
    "rodar",
    "executar",
    "implantar",
    "ler",
    "escrever",
    "construir",
    "corrigir",
    "consertar",
    "atualizar",
    "excluir",
    "apagar",
    "deletar",
    "instalar",
    "configurar",
    "verificar",
    "checar",
    "testar",
    "compilar",
    "baixar",
    "enviar",
    "mostrar",
    "exibir",
    "abrir",
    "fechar",
    "iniciar",
    "começar",
    "parar",
    "reiniciar",
    "analisar",
    "explicar",
    "me ajude",
    "por favor",
    "pode",
    "poderia",
    "adicionar",
    "remover",
    "mover",
    "copiar",
    "renomear",
    "mesclar",
    "dividir",
    "ordenar",
    "filtrar",
    "calcular",
    "converter",
    "formatar",
    "comprimir",
    "criptografar",
    "conectar",
    "sincronizar",
    "fazer backup",
    "restaurar",
    "monitorar",
    "exportar",
    "importar",
    // German
    "suchen",
    "finden",
    "erstellen",
    "ausführen",
    "bereitstellen",
    "lesen",
    "schreiben",
    "bauen",
    "reparieren",
    "beheben",
    "aktualisieren",
    "löschen",
    "entfernen",
    "installieren",
    "konfigurieren",
    "einrichten",
    "prüfen",
    "testen",
    "kompilieren",
    "herunterladen",
    "hochladen",
    "senden",
    "anzeigen",
    "zeigen",
    "öffnen",
    "schließen",
    "starten",
    "stoppen",
    "beenden",
    "neustarten",
    "analysieren",
    "erklären",
    "hilf mir",
    "bitte",
    "kannst du",
    "könntest du",
    "hinzufügen",
    "entfernen",
    "verschieben",
    "kopieren",
    "umbenennen",
    "zusammenführen",
    "aufteilen",
    "sortieren",
    "filtern",
    "berechnen",
    "konvertieren",
    "formatieren",
    "komprimieren",
    "verschlüsseln",
    "verbinden",
    "synchronisieren",
    "sichern",
    "wiederherstellen",
    "überwachen",
    "exportieren",
    "importieren",
    // Russian
    "найти",
    "создать",
    "запустить",
    "развернуть",
    "читать",
    "прочитать",
    "написать",
    "записать",
    "искать",
    "собрать",
    "исправить",
    "починить",
    "обновить",
    "удалить",
    "убрать",
    "установить",
    "поставить",
    "настроить",
    "сконфигурировать",
    "проверить",
    "тестировать",
    "скомпилировать",
    "скачать",
    "загрузить",
    "отправить",
    "послать",
    "показать",
    "вывести",
    "открыть",
    "закрыть",
    "запустить",
    "остановить",
    "перезапустить",
    "анализировать",
    "объяснить",
    "помоги",
    "пожалуйста",
    "можешь",
    "мог бы",
    "добавить",
    "убрать",
    "переместить",
    "копировать",
    "переименовать",
    "объединить",
    "разделить",
    "отсортировать",
    "отфильтровать",
    "посчитать",
    "вычислить",
    "конвертировать",
    "форматировать",
    "сжать",
    "зашифровать",
    "подключить",
    "синхронизировать",
    "сделать бэкап",
    "восстановить",
    "мониторить",
    "экспортировать",
    "импортировать",
    // Arabic
    "ابحث",
    "أنشئ",
    "شغّل",
    "انشر",
    "اقرأ",
    "اكتب",
    "ابحث",
    "ابنِ",
    "أصلح",
    "حدّث",
    "احذف",
    "ثبّت",
    "اضبط",
    "افحص",
    "اختبر",
    "حمّل",
    "ارفع",
    "أرسل",
    "اعرض",
    "افتح",
    "أغلق",
    "ابدأ",
    "أوقف",
    "أعد التشغيل",
    "حلّل",
    "اشرح",
    "ساعدني",
    "من فضلك",
    "هل يمكنك",
    "أضف",
    "أزل",
    "انقل",
    "انسخ",
    "أعد التسمية",
    "ادمج",
    "قسّم",
    "رتّب",
    "صفّ",
    "احسب",
    "حوّل",
    "نسّق",
    "اضغط",
    "شفّر",
    "اتصل",
    "زامن",
    "انسخ احتياطياً",
    "استعد",
    "راقب",
    "صدّر",
    "استورد",
    // Hindi
    "खोजें",
    "बनाएं",
    "चलाएं",
    "तैनात करें",
    "पढ़ें",
    "लिखें",
    "खोज करें",
    "बनाना",
    "ठीक करें",
    "सुधारें",
    "अपडेट करें",
    "हटाएं",
    "मिटाएं",
    "इंस्टॉल करें",
    "सेटअप करें",
    "जांचें",
    "टेस्ट करें",
    "कंपाइल करें",
    "डाउनलोड करें",
    "अपलोड करें",
    "भेजें",
    "दिखाएं",
    "खोलें",
    "बंद करें",
    "शुरू करें",
    "रोकें",
    "रीस्टार्ट करें",
    "विश्लेषण करें",
    "समझाएं",
    "मदद करो",
    "मदद कीजिए",
    "कृपया",
    "क्या आप",
    "जोड़ें",
    "निकालें",
    "हटाएं",
    "कॉपी करें",
    "नाम बदलें",
    "मर्ज करें",
    "विभाजित करें",
    "क्रमबद्ध करें",
    "गणना करें",
    "बदलें",
    "फॉर्मैट करें",
    "कनेक्ट करें",
    "सिंक करें",
    "बैकअप करें",
    "रिस्टोर करें",
    "मॉनिटर करें",
    // Thai
    "หา",
    "ค้นหา",
    "สร้าง",
    "รัน",
    "เดพลอย",
    "อ่าน",
    "เขียน",
    "ค้น",
    "สร้าง",
    "แก้",
    "แก้ไข",
    "อัปเดต",
    "ลบ",
    "ติดตั้ง",
    "ตั้งค่า",
    "ตรวจสอบ",
    "เช็ค",
    "ทดสอบ",
    "คอมไพล์",
    "ดาวน์โหลด",
    "อัปโหลด",
    "ส่ง",
    "แสดง",
    "ดู",
    "เปิด",
    "ปิด",
    "เริ่ม",
    "หยุด",
    "รีสตาร์ท",
    "วิเคราะห์",
    "อธิบาย",
    "ช่วย",
    "ได้ไหม",
    "เพิ่ม",
    "ลบ",
    "ย้าย",
    "คัดลอก",
    "เปลี่ยนชื่อ",
    "รวม",
    "แยก",
    "เรียง",
    "กรอง",
    "คำนวณ",
    "แปลง",
    "บีบอัด",
    "เข้ารหัส",
    "เชื่อมต่อ",
    "ซิงค์",
    "สำรอง",
    "กู้คืน",
    "ติดตาม",
    // Indonesian / Malay
    "cari",
    "buat",
    "jalankan",
    "deploy",
    "baca",
    "tulis",
    "cari",
    "bangun",
    "perbaiki",
    "perbarui",
    "hapus",
    "pasang",
    "instal",
    "konfigurasi",
    "atur",
    "periksa",
    "cek",
    "uji",
    "tes",
    "kompilasi",
    "unduh",
    "download",
    "unggah",
    "upload",
    "kirim",
    "tampilkan",
    "lihat",
    "buka",
    "tutup",
    "mulai",
    "hentikan",
    "restart",
    "analisis",
    "jelaskan",
    "tolong",
    "bantu",
    "bisa",
    "tambah",
    "buang",
    "pindah",
    "salin",
    "ganti nama",
    "gabung",
    "pisah",
    "urutkan",
    "filter",
    "hitung",
    "konversi",
    "format",
    "kompres",
    "enkripsi",
    "hubungkan",
    "sinkronkan",
    "cadangkan",
    "pulihkan",
    "pantau",
    "ekspor",
    "impor",
    // Turkish
    "bul",
    "oluştur",
    "çalıştır",
    "dağıt",
    "oku",
    "yaz",
    "ara",
    "kur",
    "düzelt",
    "güncelle",
    "sil",
    "yükle",
    "kur",
    "yapılandır",
    "ayarla",
    "kontrol et",
    "test et",
    "derle",
    "indir",
    "yükle",
    "gönder",
    "göster",
    "listele",
    "aç",
    "kapat",
    "başlat",
    "durdur",
    "yeniden başlat",
    "analiz et",
    "açıkla",
    "yardım et",
    "lütfen",
    "ekle",
    "kaldır",
    "taşı",
    "kopyala",
    "yeniden adlandır",
    "birleştir",
    "böl",
    "sırala",
    "filtrele",
    "hesapla",
    "dönüştür",
    "biçimlendir",
    "sıkıştır",
    "şifrele",
    "bağlan",
    "senkronize et",
    "yedekle",
    "geri yükle",
    "izle",
    "dışa aktar",
    "içe aktar",
    // Polish
    "znajdź",
    "szukaj",
    "utwórz",
    "stwórz",
    "uruchom",
    "wdróż",
    "czytaj",
    "przeczytaj",
    "napisz",
    "zapisz",
    "zbuduj",
    "napraw",
    "zaktualizuj",
    "usuń",
    "skasuj",
    "zainstaluj",
    "skonfiguruj",
    "ustaw",
    "sprawdź",
    "przetestuj",
    "skompiluj",
    "pobierz",
    "prześlij",
    "wyślij",
    "pokaż",
    "wyświetl",
    "otwórz",
    "zamknij",
    "uruchom",
    "zatrzymaj",
    "restartuj",
    "analizuj",
    "wyjaśnij",
    "pomóż",
    "proszę",
    "czy możesz",
    "dodaj",
    "usuń",
    "przenieś",
    "kopiuj",
    "zmień nazwę",
    "połącz",
    "podziel",
    "sortuj",
    "filtruj",
    "oblicz",
    "konwertuj",
    "formatuj",
    "skompresuj",
    "zaszyfruj",
    "połącz",
    "synchronizuj",
    "zrób backup",
    "przywróć",
    "monitoruj",
    "eksportuj",
    "importuj",
    // Dutch
    "zoek",
    "vind",
    "maak",
    "aanmaken",
    "uitvoeren",
    "draaien",
    "deployen",
    "lees",
    "schrijf",
    "bouwen",
    "repareren",
    "fixen",
    "bijwerken",
    "updaten",
    "verwijderen",
    "wissen",
    "installeren",
    "configureren",
    "instellen",
    "controleren",
    "checken",
    "testen",
    "compileren",
    "downloaden",
    "uploaden",
    "verzenden",
    "sturen",
    "tonen",
    "laten zien",
    "openen",
    "sluiten",
    "starten",
    "stoppen",
    "herstarten",
    "analyseren",
    "uitleggen",
    "help me",
    "alsjeblieft",
    "kun je",
    "toevoegen",
    "verplaatsen",
    "kopiëren",
    "hernoemen",
    "samenvoegen",
    "splitsen",
    "sorteren",
    "filteren",
    "berekenen",
    "converteren",
    "formatteren",
    "comprimeren",
    "versleutelen",
    "verbinden",
    "synchroniseren",
    "back-uppen",
    "herstellen",
    "monitoren",
    "exporteren",
    "importeren",
    // Italian
    "cercare",
    "trovare",
    "creare",
    "eseguire",
    "distribuire",
    "leggere",
    "scrivere",
    "costruire",
    "correggere",
    "riparare",
    "aggiornare",
    "eliminare",
    "cancellare",
    "installare",
    "configurare",
    "impostare",
    "controllare",
    "verificare",
    "testare",
    "compilare",
    "scaricare",
    "caricare",
    "inviare",
    "mostrare",
    "visualizzare",
    "aprire",
    "chiudere",
    "avviare",
    "fermare",
    "riavviare",
    "analizzare",
    "spiegare",
    "aiutami",
    "per favore",
    "puoi",
    "aggiungere",
    "rimuovere",
    "spostare",
    "copiare",
    "rinominare",
    "unire",
    "dividere",
    "ordinare",
    "filtrare",
    "calcolare",
    "convertire",
    "formattare",
    "comprimere",
    "crittografare",
    "connettere",
    "sincronizzare",
    "eseguire backup",
    "ripristinare",
    "monitorare",
    "esportare",
    "importare",
    // Ukrainian
    "знайти",
    "шукати",
    "створити",
    "запустити",
    "розгорнути",
    "читати",
    "прочитати",
    "написати",
    "записати",
    "побудувати",
    "виправити",
    "полагодити",
    "оновити",
    "видалити",
    "встановити",
    "налаштувати",
    "перевірити",
    "протестувати",
    "скомпілювати",
    "завантажити",
    "вивантажити",
    "надіслати",
    "показати",
    "відкрити",
    "закрити",
    "запустити",
    "зупинити",
    "перезапустити",
    "аналізувати",
    "пояснити",
    "допоможи",
    "будь ласка",
    "чи можеш",
    "додати",
    "прибрати",
    "перемістити",
    "копіювати",
    "перейменувати",
    "об'єднати",
    "розділити",
    "відсортувати",
    "відфільтрувати",
    "обчислити",
    "конвертувати",
    "форматувати",
    "стиснути",
    "зашифрувати",
    "підключити",
    "синхронізувати",
    "зробити бекап",
    "відновити",
    "моніторити",
    "експортувати",
    "імпортувати",
    // Swedish
    "hitta",
    "sök",
    "skapa",
    "köra",
    "kör",
    "distribuera",
    "läsa",
    "läs",
    "skriva",
    "skriv",
    "bygga",
    "fixa",
    "laga",
    "uppdatera",
    "radera",
    "ta bort",
    "installera",
    "konfigurera",
    "ställ in",
    "kontrollera",
    "kolla",
    "testa",
    "kompilera",
    "ladda ner",
    "ladda upp",
    "skicka",
    "visa",
    "öppna",
    "stänga",
    "starta",
    "stoppa",
    "starta om",
    "analysera",
    "förklara",
    "hjälp mig",
    "snälla",
    "kan du",
    "lägg till",
    "flytta",
    "kopiera",
    "byt namn",
    "slå ihop",
    "dela",
    "sortera",
    "filtrera",
    "beräkna",
    "konvertera",
    "formatera",
    "komprimera",
    "kryptera",
    "anslut",
    "synkronisera",
    "säkerhetskopiera",
    "återställa",
    "övervaka",
    "exportera",
    "importera",
];

/// Maximum message length in chars for a trivial classification.
const TRIVIAL_MAX_LEN: usize = 50;

/// Maximum task description length (in chars) for a task to be considered Simple.
const SIMPLE_DESCRIPTION_MAX_LEN: usize = 100;

/// History length threshold above which a conversation is considered complex.
const COMPLEX_HISTORY_THRESHOLD: usize = 10;

// ── Enums ────────────────────────────────────────────────────────────────

/// Task complexity level as determined by rule-based classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TaskComplexity {
    /// Trivial: pure conversation, no tools needed.
    /// Greetings, thanks, one-word responses, simple questions with no action verbs.
    Trivial,
    /// Simple: single read-only tool, short description, shallow history.
    Simple,
    /// Standard: the default bucket for everything that is neither
    /// clearly simple nor clearly complex.
    Standard,
    /// Complex: architecture/debug/refactor tasks, deep history, compound
    /// tool usage, or DONE criteria present.
    Complex,
}

impl TaskComplexity {
    /// Get the execution profile for this complexity level.
    pub fn execution_profile(&self) -> ExecutionProfile {
        match self {
            TaskComplexity::Trivial => ExecutionProfile::trivial(),
            TaskComplexity::Simple => ExecutionProfile::simple(),
            TaskComplexity::Standard => ExecutionProfile::standard(),
            TaskComplexity::Complex => ExecutionProfile::complex(),
        }
    }
}

/// Model tier that maps to a configured model name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelTier {
    /// Fastest / cheapest model for trivial tasks.
    Fast,
    /// Default model — the primary workhorse.
    Primary,
    /// Most capable model for hard tasks.
    Premium,
}

// ── User override prefixes ───────────────────────────────────────────────

/// Prefix that forces the Fast tier.
const FORCE_FAST_PREFIX: &str = "!fast";

/// Prefix that forces the Premium tier.
const FORCE_BEST_PREFIX: &str = "!best";

// ── Configuration ────────────────────────────────────────────────────────

/// Configuration for the tiered model router.
///
/// Can be embedded in the agent section of `temm1e.toml` or supplied
/// programmatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRouterConfig {
    /// Whether tiered routing is enabled. When `false`, all requests use
    /// the primary model regardless of complexity.
    #[serde(default)]
    pub enabled: bool,

    /// Model name for the Fast tier (e.g. `"claude-haiku-4-5-20251001"`).
    /// If `None`, Fast-tier tasks fall back to the primary model.
    #[serde(default)]
    pub fast_model: Option<String>,

    /// Model name for the Primary tier. This is the default model from
    /// the provider config and must always be set.
    #[serde(default = "default_primary_model")]
    pub primary_model: String,

    /// Model name for the Premium tier (e.g. `"claude-opus-4-6"`).
    /// If `None`, Premium-tier tasks fall back to the primary model.
    #[serde(default)]
    pub premium_model: Option<String>,
}

fn default_primary_model() -> String {
    "claude-sonnet-4-6".to_string()
}

impl Default for ModelRouterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fast_model: None,
            primary_model: default_primary_model(),
            premium_model: None,
        }
    }
}

// ── Router ───────────────────────────────────────────────────────────────

/// Rule-based model router. Classifies task complexity and selects the
/// appropriate model tier without making any LLM calls.
#[derive(Debug, Clone)]
pub struct ModelRouter {
    config: ModelRouterConfig,
}

impl ModelRouter {
    /// Create a new `ModelRouter` from configuration.
    pub fn new(config: ModelRouterConfig) -> Self {
        Self { config }
    }

    /// Whether the router is enabled. When disabled, [`get_model_name`]
    /// always returns the primary model.
    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    /// Convenience method: classify, select tier, and return the model name
    /// in one call.
    ///
    /// * `history` — conversation history so far.
    /// * `tool_names` — names of tools used (or requested) in this turn.
    /// * `task_description` — the user's message text.
    /// * `is_verification` — whether this is a verification step (Phase 1.1).
    pub fn route(
        &self,
        history: &[ChatMessage],
        tool_names: &[&str],
        task_description: &str,
        is_verification: bool,
    ) -> &str {
        if !self.config.enabled {
            return &self.config.primary_model;
        }

        // Verification steps always use Primary or Premium.
        if is_verification {
            let tier = if self.config.premium_model.is_some() {
                ModelTier::Premium
            } else {
                ModelTier::Primary
            };
            let model = self.get_model_name(tier);
            debug!(
                tier = ?tier,
                model = %model,
                "Verification step — using elevated tier"
            );
            return model;
        }

        // Check for user overrides.
        if let Some(forced) = Self::detect_user_override(task_description) {
            let model = self.get_model_name(forced);
            info!(
                tier = ?forced,
                model = %model,
                "User override detected"
            );
            return model;
        }

        let complexity = self.classify_complexity(history, tool_names, task_description);
        let tier = Self::select_tier(complexity);
        let model = self.get_model_name(tier);

        info!(
            complexity = ?complexity,
            tier = ?tier,
            model = %model,
            "Routed task to model"
        );

        model
    }

    /// Classify the complexity of a task based on conversation history,
    /// tool usage, and task description. Entirely rule-based.
    pub fn classify_complexity(
        &self,
        history: &[ChatMessage],
        tool_names: &[&str],
        task_description: &str,
    ) -> TaskComplexity {
        let desc_lower = task_description.to_lowercase();

        // ── Trivial signals ─────────────────────────────────────────
        let short_msg = task_description.len() <= TRIVIAL_MAX_LEN;
        let no_tools = tool_names.is_empty();
        let shallow_history = history.len() <= 3;
        let has_action_verb = ACTION_VERBS.iter().any(|v| desc_lower.contains(v));
        let is_greeting = TRIVIAL_PATTERNS
            .iter()
            .any(|p| desc_lower.trim() == *p || desc_lower.starts_with(p));
        let has_path_or_url =
            desc_lower.contains('/') || desc_lower.contains("http") || desc_lower.contains("```");

        if short_msg
            && no_tools
            && (shallow_history || is_greeting)
            && !has_action_verb
            && !has_path_or_url
        {
            return TaskComplexity::Trivial;
        }

        // ── Complex signals ──────────────────────────────────────────

        // 1. Keywords indicating complex work.
        let has_complex_keyword = COMPLEX_KEYWORDS.iter().any(|kw| desc_lower.contains(kw));

        // 2. Deep conversation history.
        let deep_history = history.len() > COMPLEX_HISTORY_THRESHOLD;

        // 3. Multiple distinct tool types used.
        let unique_tools: std::collections::HashSet<&str> = tool_names.iter().copied().collect();
        let multi_tool_types = unique_tools.len() > 2;

        // 4. DONE criteria present (compound task).
        let has_done_criteria = desc_lower.contains("done criteria")
            || desc_lower.contains("done when")
            || desc_lower.contains("acceptance criteria")
            || self.history_contains_done_criteria(history);

        if has_complex_keyword || (deep_history && multi_tool_types) || has_done_criteria {
            return TaskComplexity::Complex;
        }

        // ── Simple signals ───────────────────────────────────────────

        // Short description.
        let short_description = task_description.len() < SIMPLE_DESCRIPTION_MAX_LEN;

        // Single tool call (or zero).
        let single_tool = tool_names.len() <= 1;

        // All requested tools are read-only.
        let all_read_only = tool_names.iter().all(|t| READ_ONLY_TOOLS.contains(t));

        // Count action verbs — multiple verbs signal a compound task that
        // should never be Simple (e.g., "open youtube, search news, then summarize").
        let action_verb_count = ACTION_VERBS
            .iter()
            .filter(|v| desc_lower.contains(*v))
            .count();
        let is_compound = action_verb_count >= 2;

        if short_description && single_tool && all_read_only && !deep_history && !is_compound {
            return TaskComplexity::Simple;
        }

        // ── Default ──────────────────────────────────────────────────
        TaskComplexity::Standard
    }

    /// Map a complexity level to a model tier.
    pub fn select_tier(complexity: TaskComplexity) -> ModelTier {
        match complexity {
            TaskComplexity::Trivial => ModelTier::Fast,
            TaskComplexity::Simple => ModelTier::Fast,
            TaskComplexity::Standard => ModelTier::Primary,
            TaskComplexity::Complex => ModelTier::Premium,
        }
    }

    /// Resolve a model tier to a concrete model name string, falling
    /// back to the primary model if a tier's model is not configured.
    pub fn get_model_name(&self, tier: ModelTier) -> &str {
        match tier {
            ModelTier::Fast => self
                .config
                .fast_model
                .as_deref()
                .unwrap_or(&self.config.primary_model),
            ModelTier::Primary => &self.config.primary_model,
            ModelTier::Premium => self
                .config
                .premium_model
                .as_deref()
                .unwrap_or(&self.config.primary_model),
        }
    }

    /// Detect user override prefixes (`!fast`, `!best`) in the task
    /// description. Returns `Some(tier)` if an override is found.
    fn detect_user_override(task_description: &str) -> Option<ModelTier> {
        let trimmed = task_description.trim_start();
        if trimmed.starts_with(FORCE_FAST_PREFIX) {
            Some(ModelTier::Fast)
        } else if trimmed.starts_with(FORCE_BEST_PREFIX) {
            Some(ModelTier::Premium)
        } else {
            None
        }
    }

    /// Check whether the conversation history already contains DONE
    /// criteria injected by the done-criteria engine.
    fn history_contains_done_criteria(&self, history: &[ChatMessage]) -> bool {
        for msg in history {
            if !matches!(msg.role, Role::System) {
                continue;
            }
            let text = match &msg.content {
                MessageContent::Text(t) => t.as_str(),
                MessageContent::Parts(parts) => {
                    // Check each text part.
                    for part in parts {
                        if let ContentPart::Text { text } = part {
                            let lower = text.to_lowercase();
                            if lower.contains("done criteria")
                                || lower.contains("completion conditions")
                            {
                                return true;
                            }
                        }
                    }
                    continue;
                }
            };
            let lower = text.to_lowercase();
            if lower.contains("done criteria") || lower.contains("completion conditions") {
                return true;
            }
        }
        false
    }
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_core::types::message::{ChatMessage, MessageContent, Role};

    fn make_config(fast: Option<&str>, primary: &str, premium: Option<&str>) -> ModelRouterConfig {
        ModelRouterConfig {
            enabled: true,
            fast_model: fast.map(|s| s.to_string()),
            primary_model: primary.to_string(),
            premium_model: premium.map(|s| s.to_string()),
        }
    }

    fn make_router() -> ModelRouter {
        ModelRouter::new(make_config(
            Some("claude-haiku-4-5-20251001"),
            "claude-sonnet-4-6",
            Some("claude-opus-4-6"),
        ))
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn assistant_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text(text.to_string()),
        }
    }

    fn system_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: Role::System,
            content: MessageContent::Text(text.to_string()),
        }
    }

    // ── Classification tests ─────────────────────────────────────────

    #[test]
    fn simple_task_short_readonly() {
        let router = make_router();
        let history = vec![user_msg("show me the file")];
        let complexity = router.classify_complexity(&history, &["file_read"], "show me the file");
        assert_eq!(complexity, TaskComplexity::Simple);
    }

    #[test]
    fn simple_task_no_tools() {
        let router = make_router();
        let history = vec![user_msg("hi")];
        let complexity = router.classify_complexity(&history, &[], "hi");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn simple_task_git_status() {
        let router = make_router();
        let history = vec![user_msg("git status")];
        let complexity = router.classify_complexity(&history, &["git_status"], "git status");
        assert_eq!(complexity, TaskComplexity::Simple);
    }

    #[test]
    fn standard_task_long_description() {
        let router = make_router();
        let long_desc = "Please read the configuration file and then update the database \
                         connection string to point to the new staging server at db.staging.internal";
        let history = vec![user_msg(long_desc)];
        let complexity = router.classify_complexity(&history, &["file_read"], long_desc);
        assert_eq!(complexity, TaskComplexity::Standard);
    }

    #[test]
    fn standard_task_write_tool() {
        let router = make_router();
        let history = vec![user_msg("write hello to file.txt")];
        let complexity =
            router.classify_complexity(&history, &["file_write"], "write hello to file.txt");
        assert_eq!(complexity, TaskComplexity::Standard);
    }

    #[test]
    fn complex_task_architecture_keyword() {
        let router = make_router();
        let history = vec![user_msg("design the architecture for the new auth system")];
        let complexity = router.classify_complexity(
            &history,
            &["shell"],
            "design the architecture for the new auth system",
        );
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn complex_task_debug_keyword() {
        let router = make_router();
        let history = vec![user_msg("debug the memory leak in the worker pool")];
        let complexity = router.classify_complexity(
            &history,
            &["shell"],
            "debug the memory leak in the worker pool",
        );
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn complex_task_refactor_keyword() {
        let router = make_router();
        let history = vec![user_msg("refactor the error handling")];
        let complexity =
            router.classify_complexity(&history, &["shell"], "refactor the error handling");
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn complex_task_deep_history_multi_tools() {
        let router = make_router();
        // Build a history with > 10 messages.
        let mut history = Vec::new();
        for i in 0..12 {
            history.push(user_msg(&format!("message {}", i)));
            history.push(assistant_msg(&format!("reply {}", i)));
        }
        let complexity = router.classify_complexity(
            &history,
            &["shell", "file_write", "git_status"],
            "continue working on the task",
        );
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn complex_task_done_criteria_in_description() {
        let router = make_router();
        let history = vec![user_msg("done criteria: all tests pass")];
        let complexity =
            router.classify_complexity(&history, &["shell"], "done criteria: all tests pass");
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn complex_task_done_criteria_in_history() {
        let router = make_router();
        let history = vec![
            user_msg("build a REST API"),
            system_msg("DONE CRITERIA: 1. Server starts. 2. Tests pass."),
            assistant_msg("I'll start building it."),
        ];
        let complexity = router.classify_complexity(&history, &["shell"], "continue");
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    // ── Tier selection tests ─────────────────────────────────────────

    #[test]
    fn select_tier_simple_maps_to_fast() {
        assert_eq!(
            ModelRouter::select_tier(TaskComplexity::Simple),
            ModelTier::Fast
        );
    }

    #[test]
    fn select_tier_standard_maps_to_primary() {
        assert_eq!(
            ModelRouter::select_tier(TaskComplexity::Standard),
            ModelTier::Primary
        );
    }

    #[test]
    fn select_tier_complex_maps_to_premium() {
        assert_eq!(
            ModelRouter::select_tier(TaskComplexity::Complex),
            ModelTier::Premium
        );
    }

    // ── Model name lookup tests ──────────────────────────────────────

    #[test]
    fn get_model_name_with_all_tiers_configured() {
        let router = make_router();
        assert_eq!(
            router.get_model_name(ModelTier::Fast),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(
            router.get_model_name(ModelTier::Primary),
            "claude-sonnet-4-6"
        );
        assert_eq!(router.get_model_name(ModelTier::Premium), "claude-opus-4-6");
    }

    #[test]
    fn get_model_name_fast_falls_back_to_primary() {
        let router = ModelRouter::new(make_config(
            None,
            "claude-sonnet-4-6",
            Some("claude-opus-4-6"),
        ));
        assert_eq!(router.get_model_name(ModelTier::Fast), "claude-sonnet-4-6");
    }

    #[test]
    fn get_model_name_premium_falls_back_to_primary() {
        let router = ModelRouter::new(make_config(
            Some("claude-haiku-4-5-20251001"),
            "claude-sonnet-4-6",
            None,
        ));
        assert_eq!(
            router.get_model_name(ModelTier::Premium),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn get_model_name_all_fallback_to_primary() {
        let router = ModelRouter::new(make_config(None, "claude-sonnet-4-6", None));
        assert_eq!(router.get_model_name(ModelTier::Fast), "claude-sonnet-4-6");
        assert_eq!(
            router.get_model_name(ModelTier::Primary),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            router.get_model_name(ModelTier::Premium),
            "claude-sonnet-4-6"
        );
    }

    // ── User override tests ──────────────────────────────────────────

    #[test]
    fn user_override_fast() {
        assert_eq!(
            ModelRouter::detect_user_override("!fast read the logs"),
            Some(ModelTier::Fast)
        );
    }

    #[test]
    fn user_override_best() {
        assert_eq!(
            ModelRouter::detect_user_override("!best redesign the API layer"),
            Some(ModelTier::Premium)
        );
    }

    #[test]
    fn user_override_none() {
        assert_eq!(
            ModelRouter::detect_user_override("just a normal message"),
            None
        );
    }

    #[test]
    fn user_override_with_leading_whitespace() {
        assert_eq!(
            ModelRouter::detect_user_override("  !fast check status"),
            Some(ModelTier::Fast)
        );
    }

    // ── Verification always uses Primary or Premium ──────────────────

    #[test]
    fn verification_uses_premium_when_available() {
        let router = make_router();
        let model = router.route(&[], &[], "verify the output", true);
        assert_eq!(model, "claude-opus-4-6");
    }

    #[test]
    fn verification_uses_primary_when_no_premium() {
        let router = ModelRouter::new(make_config(
            Some("claude-haiku-4-5-20251001"),
            "claude-sonnet-4-6",
            None,
        ));
        let model = router.route(&[], &[], "verify the output", true);
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn verification_ignores_user_override() {
        let router = make_router();
        // Even with !fast prefix, verification should use premium.
        let model = router.route(&[], &[], "!fast verify", true);
        assert_eq!(model, "claude-opus-4-6");
    }

    // ── Disabled router always returns primary ───────────────────────

    #[test]
    fn disabled_router_returns_primary() {
        let config = ModelRouterConfig {
            enabled: false,
            fast_model: Some("claude-haiku-4-5-20251001".to_string()),
            primary_model: "claude-sonnet-4-6".to_string(),
            premium_model: Some("claude-opus-4-6".to_string()),
        };
        let router = ModelRouter::new(config);
        let model = router.route(&[], &[], "debug the architecture", false);
        assert_eq!(model, "claude-sonnet-4-6");
    }

    // ── Route integration tests ──────────────────────────────────────

    #[test]
    fn route_simple_task_to_fast_model() {
        let router = make_router();
        let history = vec![user_msg("git status")];
        let model = router.route(&history, &["git_status"], "git status", false);
        assert_eq!(model, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn route_standard_task_to_primary_model() {
        let router = make_router();
        let history = vec![user_msg(
            "update the config file with new database credentials",
        )];
        let model = router.route(
            &history,
            &["file_write"],
            "update the config file with new database credentials",
            false,
        );
        assert_eq!(model, "claude-sonnet-4-6");
    }

    #[test]
    fn route_complex_task_to_premium_model() {
        let router = make_router();
        let history = vec![user_msg("refactor the authentication module")];
        let model = router.route(
            &history,
            &["shell", "file_write"],
            "refactor the authentication module",
            false,
        );
        assert_eq!(model, "claude-opus-4-6");
    }

    #[test]
    fn route_user_override_fast_overrides_complexity() {
        let router = make_router();
        // Even though "refactor" is a complex keyword, !fast forces Fast tier.
        let model = router.route(&[], &["shell"], "!fast refactor the module", false);
        assert_eq!(model, "claude-haiku-4-5-20251001");
    }

    #[test]
    fn route_user_override_best_overrides_complexity() {
        let router = make_router();
        // Even though it's a simple task, !best forces Premium tier.
        let model = router.route(&[], &["git_status"], "!best git status", false);
        assert_eq!(model, "claude-opus-4-6");
    }

    // ── Config serde tests ───────────────────────────────────────────

    #[test]
    fn config_serde_roundtrip() {
        let config = ModelRouterConfig {
            enabled: true,
            fast_model: Some("claude-haiku-4-5-20251001".to_string()),
            primary_model: "claude-sonnet-4-6".to_string(),
            premium_model: Some("claude-opus-4-6".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: ModelRouterConfig = serde_json::from_str(&json).unwrap();
        assert!(restored.enabled);
        assert_eq!(
            restored.fast_model.as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert_eq!(restored.primary_model, "claude-sonnet-4-6");
        assert_eq!(restored.premium_model.as_deref(), Some("claude-opus-4-6"));
    }

    #[test]
    fn config_default_values() {
        let config = ModelRouterConfig::default();
        assert!(!config.enabled);
        assert!(config.fast_model.is_none());
        assert_eq!(config.primary_model, "claude-sonnet-4-6");
        assert!(config.premium_model.is_none());
    }

    #[test]
    fn config_toml_roundtrip() {
        let config = ModelRouterConfig {
            enabled: true,
            fast_model: Some("claude-haiku-4-5-20251001".to_string()),
            primary_model: "claude-sonnet-4-6".to_string(),
            premium_model: None,
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let restored: ModelRouterConfig = toml::from_str(&toml_str).unwrap();
        assert!(restored.enabled);
        assert_eq!(
            restored.fast_model.as_deref(),
            Some("claude-haiku-4-5-20251001")
        );
        assert!(restored.premium_model.is_none());
    }

    #[test]
    fn config_deserialize_minimal_toml() {
        let toml_str = r#"
            enabled = true
            primary_model = "gpt-4o"
        "#;
        let config: ModelRouterConfig = toml::from_str(toml_str).unwrap();
        assert!(config.enabled);
        assert_eq!(config.primary_model, "gpt-4o");
        assert!(config.fast_model.is_none());
        assert!(config.premium_model.is_none());
    }

    // ── Edge cases ───────────────────────────────────────────────────

    #[test]
    fn empty_history_and_no_tools_is_trivial() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "hello");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn complex_keyword_case_insensitive() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &["shell"], "DEBUG the connection issue");
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    #[test]
    fn multiple_readonly_tools_still_standard() {
        // More than 1 tool → not single_tool, so not Simple even if all read-only.
        let router = make_router();
        let complexity =
            router.classify_complexity(&[], &["file_read", "git_status"], "check files");
        assert_eq!(complexity, TaskComplexity::Standard);
    }

    #[test]
    fn deep_history_alone_not_complex() {
        // Deep history without multi-tool-types should be Standard, not Complex.
        let router = make_router();
        let mut history = Vec::new();
        for i in 0..12 {
            history.push(user_msg(&format!("msg {}", i)));
            history.push(assistant_msg(&format!("reply {}", i)));
        }
        let complexity = router.classify_complexity(&history, &["shell"], "continue");
        assert_eq!(complexity, TaskComplexity::Standard);
    }

    #[test]
    fn acceptance_criteria_triggers_complex() {
        let router = make_router();
        let complexity = router.classify_complexity(
            &[],
            &["shell"],
            "build feature X. acceptance criteria: tests pass, docs updated",
        );
        assert_eq!(complexity, TaskComplexity::Complex);
    }

    // ── Trivial classification tests ────────────────────────────────

    #[test]
    fn trivial_greeting_hi() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "hi");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn trivial_greeting_thanks() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "thanks");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn trivial_short_no_action() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "cool");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn not_trivial_with_action_verb() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "help me fix this");
        assert_ne!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn not_trivial_with_path() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "read /etc/hosts");
        assert_ne!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn not_trivial_long_message() {
        let router = make_router();
        let long = "I was wondering if you could tell me a bit about how the authentication system works in this project";
        let complexity = router.classify_complexity(&[], &[], long);
        assert_ne!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn trivial_emoji_response() {
        let router = make_router();
        let complexity = router.classify_complexity(&[], &[], "\u{1f44d}");
        assert_eq!(complexity, TaskComplexity::Trivial);
    }

    #[test]
    fn execution_profile_from_complexity() {
        // P4: max_iterations + skip_tool_loop removed as dead; verify
        // remaining fields map correctly across tiers.
        assert_eq!(
            TaskComplexity::Trivial.execution_profile().prompt_tier,
            temm1e_core::types::optimization::PromptTier::Minimal
        );
        assert_eq!(
            TaskComplexity::Simple.execution_profile().prompt_tier,
            temm1e_core::types::optimization::PromptTier::Basic
        );
        assert!(TaskComplexity::Standard.execution_profile().use_learn);
        assert_eq!(
            TaskComplexity::Complex
                .execution_profile()
                .max_tool_output_chars,
            30_000
        );
    }

    #[test]
    fn select_tier_trivial_maps_to_fast() {
        assert_eq!(
            ModelRouter::select_tier(TaskComplexity::Trivial),
            ModelTier::Fast
        );
    }

    // ── Multilingual classification tests ─────────────────────────────

    #[test]
    fn trivial_vietnamese_greeting() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &[], "chào"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "cảm ơn"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "được"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "ừ"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "vâng"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "tuyệt vời"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn trivial_chinese_greeting() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &[], "你好"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "谢谢"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "好的"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "明白"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn trivial_japanese_greeting() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &[], "こんにちは"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "ありがとう"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "了解"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn trivial_korean_greeting() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &[], "안녕"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "감사합니다"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "ㅇㅋ"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn trivial_european_greetings() {
        let router = make_router();
        // Spanish
        assert_eq!(
            router.classify_complexity(&[], &[], "hola"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "gracias"),
            TaskComplexity::Trivial
        );
        // French
        assert_eq!(
            router.classify_complexity(&[], &[], "bonjour"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "merci"),
            TaskComplexity::Trivial
        );
        // German
        assert_eq!(
            router.classify_complexity(&[], &[], "danke"),
            TaskComplexity::Trivial
        );
        // Russian
        assert_eq!(
            router.classify_complexity(&[], &[], "привет"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "спасибо"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn trivial_arabic_hindi_thai() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &[], "مرحبا"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "شكرا"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "नमस्ते"),
            TaskComplexity::Trivial
        );
        assert_eq!(
            router.classify_complexity(&[], &[], "สวัสดี"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn complex_vietnamese_keywords() {
        let router = make_router();
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "gỡ lỗi module xác thực"),
            TaskComplexity::Complex
        );
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "tái cấu trúc hệ thống"),
            TaskComplexity::Complex
        );
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "tối ưu hóa hiệu suất"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn complex_multilingual_keywords() {
        let router = make_router();
        // Chinese
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "重构认证模块"),
            TaskComplexity::Complex
        );
        // Japanese
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "リファクタリングして"),
            TaskComplexity::Complex
        );
        // Korean
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "리팩토링 해주세요"),
            TaskComplexity::Complex
        );
        // Spanish
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "refactorizar el módulo"),
            TaskComplexity::Complex
        );
        // Russian
        assert_eq!(
            router.classify_complexity(&[], &["shell"], "рефакторинг авторизации"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn not_trivial_vietnamese_action_verb() {
        let router = make_router();
        assert_ne!(
            router.classify_complexity(&[], &[], "tạo file mới"),
            TaskComplexity::Trivial
        );
        assert_ne!(
            router.classify_complexity(&[], &[], "giúp tôi sửa lỗi"),
            TaskComplexity::Trivial
        );
    }

    #[test]
    fn not_trivial_multilingual_action_verbs() {
        let router = make_router();
        // Chinese
        assert_ne!(
            router.classify_complexity(&[], &[], "帮我创建一个文件"),
            TaskComplexity::Trivial
        );
        // Japanese
        assert_ne!(
            router.classify_complexity(&[], &[], "ファイルを作成して"),
            TaskComplexity::Trivial
        );
        // Spanish
        assert_ne!(
            router.classify_complexity(&[], &[], "crear un archivo"),
            TaskComplexity::Trivial
        );
        // French
        assert_ne!(
            router.classify_complexity(&[], &[], "créer un fichier"),
            TaskComplexity::Trivial
        );
    }

    // -- Compound task detection (multiple action verbs → Standard, not Simple) --

    #[test]
    fn compound_vietnamese_task_is_standard() {
        let router = make_router();
        // "open youtube, search Iran news, then summarize" — 3 action verbs
        let complexity = router.classify_complexity(
            &[],
            &[],
            "mở youtube tìm kiếm tin tức iran cho anh rồi tổng hợp lại nhé",
        );
        assert_eq!(
            complexity,
            TaskComplexity::Standard,
            "Compound Vietnamese task with 3 action verbs should be Standard, not Simple"
        );
    }

    #[test]
    fn compound_english_task_is_standard() {
        let router = make_router();
        // "create a file, write hello, then read it back" — 3 action verbs
        let complexity = router.classify_complexity(
            &[],
            &[],
            "create a file, write hello world, then read it back",
        );
        assert_eq!(
            complexity,
            TaskComplexity::Standard,
            "Compound English task with multiple action verbs should be Standard"
        );
    }

    #[test]
    fn single_action_verb_can_be_simple() {
        let router = make_router();
        // Single action verb with no tools should still be Simple
        let complexity = router.classify_complexity(&[], &[], "read the config file");
        // "read" is an action verb, but single verb → not compound → can be Simple
        assert!(
            complexity == TaskComplexity::Simple || complexity == TaskComplexity::Standard,
            "Single action verb task should be Simple or Standard"
        );
    }
}
