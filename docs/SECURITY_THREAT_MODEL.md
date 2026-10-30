# Arca — модель угроз

Документ описывает, от чего защищают ядро Arca (рентген, стерилизатор, просмотр) и
приложение для Android, чем именно и что остаётся остаточным риском. Ссылки на тесты указывают, где защита
проверяется автоматически.

Статус «проверено» означает наличие автоматических регрессионных и мутационных тестов в составе CI-пайплайна. Спецификация обновлена с учетом релизов 0.2.x; архитектурная модель iOS вынесена в отдельное руководство ([`docs/IOS_ARCHITECTURE.md`](IOS_ARCHITECTURE.md)).

## 1. Активы

| актив | почему ценен |
|---|---|
| устройство и данные пользователя | цель эксплойтов: выполнение кода, доступ к файлам, контактам, сети |
| сетевая идентичность пользователя | IP-адрес, NTLM-хэши, факт и время открытия документа (трекинг) |
| содержимое открытого документа | конфиденциально; не должно утекать в логи, сеть или остаточную память |
| метаданные при пересылке | автор, организация, локальные пути, история правок |
| доверие к вердикту «Чистый» | ложное «чисто» опаснее отсутствия проверки |
| атрибуция и ссылки на поддержку создателей | устойчивость проекта (см. §6) |

## 2. Нарушители

1. **Автор вредоносного документа** (основной) — полностью контролирует
   байты файла: макро-трояны, эксплойты парсеров, бомбы, полиглоты, трекеры.
   Может знать исходный код Arca и строить вход под конкретную ошибку.
2. **Пассивный наблюдатель сети** — ждёт обращений документа к своим
   серверам (пиксели, удалённые шаблоны, UNC).
3. **Злонамеренный или небрежный распространитель модификации** — пытается
   удалить атрибуцию или выдать форк за оригинал.
4. **Атакующий цепочку поставок** — подмена зависимостей или Wasm-модуля.

Вне модели: скомпрометированная ОС или рут, физический доступ к
разблокированному устройству, аппаратные атаки (Rowhammer, спекулятивное
исполнение — частично ослаблены отсутствием таймера в госте), пользователь,
который сознательно открывает документ во внешней программе.

## 3. Границы доверия

```text
[файл: НЕДОВЕРЕННЫЙ] ─► приложение Android ─JNI (B0)─► arca-engine ─► arca-sandbox
                                                                 │ граница B1: ABI v2, только байты
                                                                 ▼
                              [Wasm-гость: считается СКОМПРОМЕТИРОВАННЫМ]
                                                                 │ граница B2: конверт + ограниченный декодер
                                                                 ▼
                    XRayReport / Preview ─► JSON страницами ─► UI (текст, без разметки)
```

Граница B0 — JNI: единственное место вне гостя, где есть `unsafe` (`crates/arca-jni/src/ffi.rs`).
Всё, что приходит из Java (дескрипторы, смещения, размеры страниц), проверяется как
недоверенное, а всё, что уходит в Java, — только числа-счётчики и JSON.

Хост никогда не доверяет гостю: проверяются версия ABI, статус, границы
региона результата, магия и длина конверта; декодер работает под
`DecodeLimits` и общим бюджетом памяти; размер результата ограничен по операции; хост сам
пересчитывает хэш стерильной копии и смотрит имена её частей (§4.21). Хост при этом не
всемогущ: он сам читает центральный каталог ZIP и первые байты картинки вне песочницы
(§4.10, §4.21).

## 4. Угрозы и защиты

Обозначения: ✅ реализовано и покрыто автоматическим тестом (имя указано); ◐ частично, эвристика
или обеспечено только устройством системы (линт, структура сборки), без поведенческого теста;
⏳ запланировано; ⚠️ сверка с реальным ПО или устройством не проводилась.

### 4.1 Повреждение памяти и выполнение кода в парсере

| угроза | защита | статус · тесты |
|---|---|---|
| переполнение буфера, UAF, double free в разборе ZIP/OLE2/XML/RTF/PDF | весь код — Safe Rust, `forbid(unsafe_code)`, без C-библиотек; индексация через `.get()` | ✅ `tools/check-unsafe.sh`, линты workspace |
| целочисленное переполнение размеров и смещений | `overflow-checks = true` во всех профилях; checked/saturating арифметика; `try_from` | ✅ `truncated_archives_never_panic`, `corruption_never_panics` |
| паника на битых данных (DoS) | `unwrap`/`expect`/`panic`/`unreachable` запрещены линтами; все ошибки — типизированные `Result` | ✅ `mutation_fuzz_never_panics` (структурные мутации заголовков ZIP/CFB и содержимого XML-частей по всему корпусу; инварианты вердикта, wire-раундтрип, детерминизм, идемпотентность стерилизатора; длительный прогон — workflow `fuzz`), `garbage_modules_are_rejected_without_panicking`, таблица враждебных входов с точными вердиктами (`crates/arca-xray/src/tests/adversarial.rs`, около 94 векторов по числу строк `vector!`, и `hardening.rs`, 89 тестов — счёт по исходникам, не по запуску); ◐ `cargo-fuzz` в `fuzz/` написан, но с покрытием ни разу не запускался (только через заглушку на корпусе) |
| логическая ошибка парсера под контролем атакующего | контур 2: гость без импортов, эфемерный экземпляр, лимиты | ✅ `hostile_guest_cannot_escape_via_memory_writes`, `any_import_is_forbidden` |
| ошибка в интерпретаторе `wasmi` (побег из песочницы) | минимальная конфигурация: без `start`, без лишних proposals, валидация; отдельный поток; закреплённая версия, cargo-deny advisories | ◐ остаточный риск: `wasmi` содержит внутренний `unsafe` |

### 4.2 Отказ в обслуживании

| угроза | защита | статус · тесты |
|---|---|---|
| Zip-бомба (одиночная, вложенная, «лживый» размер) | коэффициент ≤ 100:1, лимит записи, общий бюджет распаковки (агрегатный коэффициент считается от длины **исходного** файла, а не от вложенного потока: вложенный архив не «разбавляет» бомбу), проверка фактического размера при распаковке, поток deflate обязан занять заявленный сжатый размер | ✅ `zip_bomb_member_rejects`, `nested_zip_bomb_rejects`, `ratio_bomb_is_refused_before_inflation`, `lying_uncompressed_size_is_detected`, `budget_is_shared_across_members`, `unread_bytes_inside_a_deflate_range_are_refused`, `guest_rejects_bombs_inside_the_sandbox` |
| XML Billion Laughs / quadratic blowup | `DOCTYPE` запрещён целиком, неописанные сущности отвергаются | ✅ `billion_laughs_is_refused_at_the_doctype`, `undeclared_entities_are_refused_even_without_doctype` |
| глубокая вложенность XML | глубина ≤ 64 | ✅ `depth_64_is_allowed_65_is_not`, `excessive_xml_depth_is_reported` |
| циклы FAT/DIFAT/каталога OLE2, общие сектора и многократное чтение | обнаружение циклов; у каждого сектора один владелец (общий сектор двух потоков — `CrossLinked`); бюджет чтения потоков за жизнь файла 1 ГиБ; число записей каталога проверяется до копирования цепочки; запрещены `/ \ : !` и NUL в именах, одинаковые имена соседей; память под пути ограничена | ✅ `fat_self_loop_is_a_cycle_not_a_hang`, `directory_cycle_is_detected`, `ole2_fat_cycle_is_rejected_not_hung`, `a_sector_shared_by_two_streams_is_refused`, `two_mini_streams_sharing_a_mini_sector_are_refused`, `rereading_streams_is_metered`, `the_directory_chain_is_counted_before_it_is_copied`, `forbidden_characters_in_names_are_refused`, `duplicate_sibling_names_are_refused_ignoring_case` |
| бесконечный цикл или экспоненциальный алгоритм | топливо CPU в песочнице; потолок топлива — фиксированная константа движка (10¹²), а в песочнице по умолчанию 4·10¹¹: размеры, которые заявляет документ, потолок не поднимают; массовые операции с памятью (`memory.fill/copy`, `table.grow`) тратят топливо и не обгоняют дедлайн | ✅ `infinite_loop_runs_out_of_fuel`, `starved_guest_is_stopped`, `starvation_is_still_caught_with_tiny_slices`, `the_fuel_ceiling_is_a_constant_no_declared_size_can_raise`, `declared_sizes_cannot_buy_a_big_budget`, `bulk_memory_operations_are_charged_fuel`, `bulk_memory_operations_do_not_step_over_the_deadline` |
| «слишком долго» на медленном устройстве при большом бюджете топлива | топливо выдаётся порциями; между порциями хост проверяет монотонный дедлайн (по умолчанию 300 с; предел, который нельзя прибавить к часам, обрезается до 7 суток, а не превращается в «без предела») | ✅ `the_deadline_stops_an_endless_guest`, `the_policy_wall_time_is_a_ceiling_that_options_cannot_raise`, `a_tighter_option_deadline_wins_over_a_looser_policy_wall_time`, `a_wall_limit_that_does_not_fit_the_clock_is_clamped_not_dropped` |
| документ нужно прервать (пользователь закрыл файл) | `CancelToken`, опрос на каждой границе порции, без убийства процесса | ✅ `cancelling_from_another_thread_stops_an_endless_guest`, `a_cancelled_token_prevents_any_execution`, `the_real_guest_can_be_cancelled_and_timed_out` |
| обработчик прогресса UI падает или зависает | паника перехватывается; обработчик вызывается только между порциями | ✅ `a_panicking_progress_callback_is_contained` (◐ зависший обработчик блокирует поток вызывающего — его надо делать неблокирующим) |
| исчерпание памяти | `min(1 ГиБ, 96 МиБ + 4 × размер входа)` на экземпляр, лимит таблиц, лимит начальной памяти; буфер под ответ гостя выделяется через `try_reserve_exact`, нехватка памяти — отказ запуска, а не падение | ✅ `memory_growth_is_capped`, `oversized_initial_memory_is_refused_at_instantiation`, `the_per_run_memory_cap_follows_the_input`, `memory_limit_scales_with_the_input_up_to_the_ceiling`, `table_growth_is_capped` |
| «лживый» размер записи ZIP: гигабайты при распаковке или усечённый поток | распаковка ровно заявленного размера (`inflate_exact`): избыток и недобор различаются, выделение не больше заявленного | ✅ `overstated_uncompressed_size_is_detected`, `truncated_deflate_stream_is_corrupt_not_a_size_error`, `empty_deflated_member_is_readable` |
| дорогой для интерпретатора документ (миллионы мелких XML-элементов) | топливо пропорционально входу (32 000/байт), дедлайн, отмена | ◐ проверка законного документа со сжатием выше ≈ 30:1 может исчерпать бюджет и дать «слишком сложно»; просмотр и стерильная копия масштабируются по результату проверки (§4.10) |
| переполнение стека рекурсией | лимит глубины рекурсии интерпретатора и стека значений, отдельный поток | ✅ `unbounded_recursion_overflows_safely` |
| огромное число записей или находок | лимиты записей ZIP, находок, потоков PDF; `ScanLimitReached`; один и тот же поток VBA не распаковывается заново для каждого из 4096 модулей, общий размер исходников макроса ограничен | ✅ `entry_count_limit`, `nesting_limit_is_enforced`, `b13_modules_that_share_a_stream_read_it_once`, `b13_the_macro_source_of_a_project_is_bounded_in_total`, `b7_overlapping_streams_cannot_multiply_the_work` |
| усиление при декодировании ответа гостя: один байт на проводе — десятки байт в памяти (`Vec::decode` с `with_capacity(n)`), строки без учёта | общий бюджет декодера wire (`DEFAULT_TOTAL_BYTES` 256 МиБ): каждая последовательность оплачивает `n × размер элемента в памяти`, каждая строка — свою длину, выделение порциями через `try_reserve` | ✅ `a_sequence_of_small_items_cannot_amplify_past_the_total_budget`, `the_total_budget_can_be_set_and_charges_strings_too`, `nested_sequences_pay_for_every_header` |
| раздувание результата: гость возвращает сотни мегабайт, JSON для приложения огромен | предел вывода по операции (отчёт, просмотр, страница — 64 МиБ; картинка — 40 МиБ; стерильная копия — 64 МиБ плюс длина файла); страница JSON ≤ 8 МиБ, отчёт ≤ 32 МиБ; ширины столбцов считаются по реальному числу столбцов | ✅ `only_a_sterile_copy_may_come_back_larger_than_a_report_can_be`, `a_page_of_huge_cells_is_refused_not_built`, `the_report_json_has_a_size_limit_too`, `a_grid_that_declares_many_columns_costs_the_columns_it_uses`, `the_widths_of_a_whole_preview_share_one_budget` |
| хост-подсказка бюджета (`sniff_inflated`) читает заявленные размеры ZIP и покупается ложью | запись засчитывается не больше чем в `сжатый × 100 + 1 МиБ` и не больше 256 МиБ, сумма — не больше 100 × файла и 1 ГиБ, больше 20 000 записей — подсказки нет; подсказка двигает только бюджет, который сам упирается в фиксированный потолок | ✅ `declared_sizes_cannot_buy_a_big_budget`, `an_honest_archive_gets_its_declared_size` |
| квадратичная работа в разборе (базы Jet: цепочки значений, столбцы, читающие одни байты; XML с >N атрибутов; Markdown; форматы ячеек; общие формулы; PDF-формы по страницам) | цепочки с множеством посещённых и счётчиком работы на обход (1 000 000 единиц на обход таблицы), проверка дубликатов атрибутов за O(n), разбор ссылок Markdown линейный, формат ячейки не интерпретируется сверх лимита, общая формула не стоит заново для каждой ячейки, бюджет форм на документ (100 000), а не на страницу | ✅ `columns_that_read_the_same_bytes_cannot_multiply_the_work_of_a_scan`, `many_attributes_with_a_duplicate_at_the_end_are_found`, `a_markdown_line_of_brackets_is_linear`, `a_format_code_of_a_million_placeholders_is_not_interpreted`, `a_shared_formula_bomb_costs_nothing_per_cell`, `the_form_budget_belongs_to_the_document_not_to_each_page` |

### 4.3 Файловая система

| угроза | защита | статус · тесты |
|---|---|---|
| Zip Slip / обход путей (`../`, абсолютные пути, `\`, буквы дисков, NUL, `:`, `%2e`/`%2f`/`%5c`, точка или пробел в конце компонента, невидимые символы Cf/Zl/Zp, теги, заполнители) | канонизация и отказ до любого использования имени; документ получает вердикт `Rejected` | ✅ `rejects_path_traversal_variants`, `path_traversal_rejects` (новые виды имён проверены в `zip/tests.rs` и `adversarial.rs`) |
| символьные ссылки в архиве | отказ по атрибутам Unix | ✅ `rejects_unix_symlinks` |
| дубликаты с разным регистром (затенение частей) | отказ | ✅ `rejects_case_insensitive_duplicates` |
| запись на диск из документа | ядро не пишет файлов; у гостя нет ФС и нет ни одного импорта; стерильная копия уходит только через системный диалог | ✅ `any_import_is_forbidden` (гость без импортов); запись копии приложением — ◐ только код и манифест, поведенческого теста нет |

### 4.4 Активное содержимое

| угроза | защита | статус · тесты |
|---|---|---|
| VBA (`vbaProject.bin`), автозапуск | никогда не исполняется; извлекается как текст для HUD; удаляется стерилизатором | ✅ `vba_macros_are_extracted_and_triaged`, `macro_project_hidden_in_a_docx_is_still_found`, `kitchen_sink_word_document_is_sterilised` |
| VBA stomping (p-code без исходника) | эвристика несоответствия | ◐ |
| XLM / Excel 4.0, `Auto_Open`, «очень скрытые» листы | обнаружение, удаление макролистов и имён | ✅ `excel4_macros_and_very_hidden_sheets`, `excel_macros_and_formulas_are_sterilised` |
| DDE в полях Word, включая обфускацию `QUOTE`, разбиение по runs и **вложенные поля** (`{ {QUOTE "DDEAUTO"} cmd }`, `REF`, `SET`, `DOCVARIABLE` на месте ключевого слова) | разбор инструкций полей со стеком вложенности: ключевое слово, поставленное вычисляемым полем, считается DDE, пока не доказано обратное; то же в бинарном Word (поля через границы кусков таблицы, вложенный `QUOTE`), в RTF (стек `\fldinst` по глубине групп, `\ucN` в пределах группы, `\fldrslt` не часть инструкции) и в стерилизаторе (те же критерии) | ✅ `dde_fields_split_and_obfuscated`, `b5_a_quote_with_a_quoted_string_supplies_the_dde_keyword`, `b5_other_nested_fields_in_keyword_position_are_dde_until_proven_otherwise`, `b5_binary_word_fields_that_straddle_two_pieces_are_found`, `b5_the_sanitizer_neutralises_what_the_scanner_calls_dde`, `b6_a_nested_fldinst_does_not_replace_the_outer_one`, `b6_uc0_means_the_character_after_u_is_text` |
| DDE и опасные функции в формулах (`WEBSERVICE`, `CALL`, `REGISTER`) | обнаружение; удаление формулы с сохранением значения | ✅ `dde_and_webservice_formulas` |
| OLE-пакеты, исполняемые вложения, опасные CLSID (Equation Editor, CVE-2017-11882) | таблица CLSID, `Ole10Native`, проверка расширений | ✅ `rtf_equation_exploit_is_detected`, `revisions_people_and_equation_ole`, `executables_inside_packages_are_flagged` |
| RTF `objdata` / `objupdate` | токенизатор RTF, заголовок OLE1 | ✅ `rtf_equation_exploit_is_detected` |
| ActiveX, callbacks ленты, `ppaction://program`, Visio `RUNADDON` (сопоставление формул Visio как в самом Visio, без учёта пробелов и регистра), привязки ODF к макросам, любой файл под `Scripts/` | обнаружение и удаление | ✅ `powerpoint_mouse_over_program_action`, `b10_visio_formulas_are_matched_the_way_visio_parses_them`, `b10_opendocument_script_urls_are_matched_without_regard_to_case`, `b10_opendocument_user_interface_bindings_can_run_macros`, `b10_any_file_under_scripts_is_a_script` |
| надстройки Office (web extension, панели задач), XLSB (`workbook.bin`, внешние ссылки `.bin`), вложения писем MSG, пакеты в двоичных частях, Visio `Hyperlink` | надстройка — исполняемое содержимое; двоичные части сканируются как контейнеры (CFB/ZIP/PDF/RTF/Jet), неопознанная двоичная часть — пробел покрытия, если это не простой медиафайл; двоичная книга и бинарный Visio не бывают `Clean`; вложения MSG и ссылки Visio учитываются | ✅ `b10_office_add_ins_are_executable_content`, `b10_binary_parts_of_an_unknown_kind_are_a_gap_unless_they_are_plain_media`, `b10_containers_hidden_in_binary_parts_are_scanned`, `b10_binary_workbooks_are_never_called_clean`, `b10_attachments_of_a_message_are_scanned`, `b10_visio_hyperlink_sections_are_external_references`, `b10_a_binary_visio_drawing_is_never_clean` |
| PDF JavaScript (в т. ч. в object streams), Launch, OpenAction, вложения, спрятанные за фильтрами потока | лексический анализ с нормализацией `#xx`; словарь потока разбирается по-настоящему (вложенный `/DecodeParms` не прячет фильтр); цепочки `ASCIIHex`/`ASCII85`/`RunLength`/`LZW`/`Flate` с предикторами; при расхождении `/Length` с реальным концом читаются оба варианта; NUL — пробельный символ; неподдержанный фильтр или поток без читаемого словаря — пробел покрытия (`Unverified`), а не пропуск | ✅ `pdf_javascript_in_object_stream`, `b7_a_nested_decodeparms_dictionary_does_not_hide_the_filter`, `b7_filter_chains_are_decoded`, `b7_a_png_predictor_is_undone`, `b7_nul_is_white_space_between_tokens`, `b7_a_lying_length_shows_both_readings`, `b7_a_filter_that_is_not_decoded_is_a_coverage_gap`, `b7_a_stream_without_a_parsable_dictionary_is_a_coverage_gap` |
| CSV-инъекция формул | обнаружение ячеек, начинающихся с `= + - @`, табуляции или CR | ✅ `csv_formula_injection` |
| текстовые файлы, которые что-то запускает по содержимому: `.url`, `.scf`, `.theme`, `.iqy`, `.library-ms`, `.settingcontent-ms`, `.sct`, `.wsf`, `.jnlp`; скрипты (VBS/JS/пакетные/PowerShell); HTML/SVG-разметка в любом тексте | распознаются по содержимому, не по имени; на верхнем уровне ярлык/сценарный файл — `DisguisedExecutable` (`Rejected`); текст со скриптом — `EmbeddedScript`; разметку (`<script>`, `onload=`, `meta refresh`, `javascript:`) ищут в любом тексте, в том числе после первых 512 знаков; формулы CSV за числом не пропускаются; HTML как формат отвергается | ✅ `b8_shortcuts_and_scripts_are_not_documents`, `b8_script_code_in_a_text_file_is_active_content`, `b8_html_after_the_first_512_characters_is_still_html`, `b8_markup_handlers_urls_and_redirects_are_found_in_any_text`, `b8_csv_formulas_behind_a_number_are_still_formulas`, `b8_sylk_formulas_are_found`, `b8_ordinary_prose_is_clean` |
| плоский XML (flat OPC, Word 2003 XML, Excel 2003 XML, плоский ODF, SVG), раньше проходивший пустым посетителем | разбор по корневому элементу; неизвестный корень — `ScanLimitReached` (`Unverified`), не `Clean` | ✅ `b8_flat_opc_is_scanned_part_by_part`, `b8_word_2003_xml_fields_are_scanned`, `b8_excel_2003_xml_formulas_are_scanned`, `b8_flat_opendocument_scripts_are_found`, `b8_an_svg_document_is_scanned`, `b8_an_xml_document_of_an_unknown_kind_is_not_clean` |
| StarBasic и события ODF | обнаружение | ✅ `odf_basic_macro_and_metadata` |
| замаскированный исполняемый файл (`.docx`, который на деле PE/ELF/Mach-O) | сигнатуры вместо расширения, вердикт `Rejected` | ✅ `disguised_executables_reject` |

### 4.5 Внешнее содержимое, SSRF, трекинг, утечки NTLM

| угроза | защита | статус · тесты |
|---|---|---|
| удалённый шаблон (`attachedTemplate`, CVE-2017-0199) | никакой загрузки; находка Critical, вердикт «обезврежено»; стерилизатор удаляет связь | ✅ `remote_template_injection` |
| трекинг-пиксель (внешняя картинка) | никакой загрузки, вердикт «скрытые трекеры» | ✅ `tracking_pixel_is_a_tracker`, `tracking_field_and_hidden_text` |
| UNC-путь (`\\host\share`) → утечка NTLM-хэша | классификация UNC, Critical для шаблонов | ✅ `unc_template_leaks_ntlm` |
| `file:`-URL, ведущий на хост (`file://host/…`, `file:////host/…`, `file:///\\host\…`) и цели `//host/…` без `TargetMode` | классификация по тому, *где находится хост*, а не по числу косых черт | ✅ `a2_file_uri_forms_that_reach_a_host_are_remote_templates`, `a2_genuinely_local_file_uris_stay_local`, `f4_unc_target_without_target_mode_is_remote`, `f4_scheme_target_without_target_mode_is_remote` |
| сетевые пути, записанные хитро: смесь `/` и `\` (`\/host`, `/\host`), `file:///%5C%5Chost`, двойное процентное кодирование; протоколы `smb:`, `ms-outlook:`, `outlook:`, `onenote:`, `ms-onenote:` и любая схема `ms-*` | классификация цели после процентного декодирования (до трёх раундов) и с любой смесью разделителей в начале; перечисленные схемы — опасные протоколы | ✅ `b11_mixed_slashes_and_escapes_are_network_paths`, `b11_a_mixed_slash_template_is_found_even_without_target_mode`, `b11_linked_media_load_by_themselves` |
| URL и DDE внутри формул (`HYPERLINK`, `WEBSERVICE`, `IMAGE`, DDE после безобидной `\|`) | URL из формул попадают в `external_refs`; DDE ищется по всей формуле | ✅ `f3_hyperlink_unc_target_is_an_ntlm_leak_finding`, `f3_dde_is_found_after_an_earlier_harmless_pipe`, `f3_csv_formula_urls_are_listed`, `f3_ods_webservice_formula_url_is_listed`, `f3_ods_dde_function_and_dde_link_are_findings`, `prefilter_is_a_superset` |
| протокольные обработчики (`ms-msdt:`, Follina) | классификация опасных схем | ✅ `follina_style_protocol_handler` |
| удалённые изображения и HTML в Markdown | обнаружение | ✅ `markdown_remote_images_and_html` |
| любое сетевое обращение ядра | в ядре нет сетевого кода; гость без импортов; приложению не нужен `INTERNET`; приложение не запрашивает ни одного разрешения, это проверяется сборкой | ✅ `any_import_is_forbidden`; манифест — ◐ проверяет `build-apk.sh` и `check_resources.py` при каждой сборке пакета, отдельного теста нет |

### 4.6 Сокрытие и обман пользователя

| угроза | защита | статус · тесты |
|---|---|---|
| полиглот (PDF+ZIP и т. п.), хвостовые данные, пакет, который одновременно OPC и ODF, `%PDF-` в первых 1024 байтах ZIP/OLE | поиск вложенных сигнатур и смещений; архив-двойник сканируется как обе интерпретации; `PolyglotFile`, `UnreferencedArchiveData` и ненулевой `TrailingData` — пробелы покрытия: `Clean` невозможен; для картинок проверка идёт по всем байтам всегда | ✅ `polyglot_pdf_zip`, `prefix_and_trailing_data_are_reported`, `b9_an_archive_that_is_a_package_and_an_opendocument_is_scanned_as_both`, `b9_a_benign_dual_format_archive_is_unverified`, `b9_trailing_zero_padding_is_harmless_but_anything_else_is_not`, `b9_unreferenced_data_between_members_is_a_gap`, `b9_a_pdf_header_near_the_start_of_an_archive_or_compound_file`, `b9_a_picture_in_a_package_is_checked_for_a_second_format` |
| расхождение парсеров (локальный заголовок ≠ центральный каталог, перекрытия, сдвиг, подложный конец каталога в комментарии записи, дескриптор данных и Unicode-имя, называющие часть иначе) | строгое согласование, вердикт `Rejected`; см. также §4.22 | ✅ `local_and_central_names_must_agree`, `overlapping_members_are_rejected`, `shifted_central_directory_is_rejected`, `a_second_end_record_hidden_in_an_entry_comment_is_refused`, `a_lying_data_descriptor_is_refused`, `a_unicode_path_extra_that_renames_the_member_is_refused` |
| скрытый и «белый» текст, скрытые листы и слайды | обнаружение, показ в HUD | ✅ `tracking_field_and_hidden_text` |
| bidi-переопределения в именах (`cod.exe` → `exe.doc`) и невидимые символы в именах | `display_safe` при показе, находка `BidiOverride`; в именах частей ZIP bidi-управляющие и невидимые символы — отказ | ✅ образец `markdown-bidi-override` в корпусе (`crates/arca-xray/src/tests/corpus.rs`); имена ZIP — `rejects_path_traversal_variants` |
| «осиротевшие» данные в OLE2/ZIP | обнаружение | ✅ `orphaned_entries_are_reported` |
| наводнение безобидными находками, вытесняющее решающую из отчёта | вердикт строится из фактов, не зависящих от лимита находок | ✅ `a1_flood_of_hyperlinks_cannot_hide_dde_in_the_body`, `a1_flood_in_the_same_rels_part_cannot_hide_remote_template`, `a1_first_finding_of_every_class_survives_the_cap`, `a1_dropped_findings_still_feed_the_verdict_facts`, `a1_auto_load_reference_beyond_the_cap_still_marks_trackers` |
| часть документа осталась непроверенной (шифрование, битый XML, лимит, вложенность, второе имя части, два формата в одном пакете, записи OLE без ссылок, неизвестный корень XML, нечитаемый каталог Access, неподдержанный фильтр PDF), но вердикт «Чистый» | вердикт `Unverified`; стерилизатор такую копию не выдаёт; перечень видов находок, делающих `Clean` невозможным, — ARCHITECTURE §4.4 | ✅ `a3_encrypted_zip_member_is_unverified_not_clean`, `a3_encrypted_pdf_is_unverified_not_clean`, `a3_nesting_limit_reached_is_unverified`, `a3_unparsable_part_is_unverified`, `a3_active_content_outranks_unverified_and_unverified_outranks_trackers`, `a3_unverified_survives_the_wire`, `b3_several_document_types_alone_are_enough_to_refuse_a_clean_verdict`, `b9_a_second_name_for_a_member_is_a_gap` |

### 4.7 Утечки приватности

| угроза | защита | статус · тесты |
|---|---|---|
| метаданные при пересылке (автор, организация, пути, принтеры, время правки) | инспектор в HUD; стерилизатор удаляет | ✅ `benign_docx_is_clean_with_metadata`, `kitchen_sink_word_document_is_sterilised` |
| ревизии и комментарии | ревизии принимаются, комментарии удаляются | ✅ |
| остаточные данные в памяти | `Zeroizing` для входа, результата, пароля, ключей AES/RC4 и ключа PDF; страж стирает всю линейную память гостя при `Drop`; пароль и буфер документа растут «с зачисткой» (старый блок стирается); `Debug` у хоста, документа, результата и ответа гостя печатает размеры | ◐ стирание добросовестное, не гарантированное: копии у аллокатора и зависимостей не контролируются; при аварийном завершении процесса страж не выполняется. Тесты: `debug_output_of_the_host_shows_neither_documents_nor_passwords`, `a_document_buffer_grows_by_doubling_and_never_past_the_limit`, `debug_output_of_a_result_shows_no_content`; самого стирания тест не наблюдает |
| логирование содержимого | ядро не логирует; `print_*` запрещены линтами | ◐ линт и обзор кода; поведенческого теста нет |
| телеметрия | отсутствует; в графе зависимостей нет сетевых крейтов; у приложения нет `INTERNET` | ◐ граф зависимостей (ARCHITECTURE §7) и манифест; на устройстве сетевой трафик в этой проверке не снимался |

### 4.8 Цепочка поставок

| угроза | защита | статус |
|---|---|---|
| подмена Wasm-модуля | `ModulePin` — SHA-256 из allow-list; модуль с другим хэшем не исполняется | ✅ `unpinned_and_tampered_modules_are_refused` |
| подмена модуля вместе с его хэшем в приложении | `Engine::pinned(wasm, expected_sha256)`: ожидаемый хэш вшивается при сборке (`arca-jni`, `arca-ffi`); `Engine::new` закрепляет модуль только его же хэшем и в приложениях не используется | ✅ `a_module_that_is_not_the_recorded_one_is_refused` (`arca-jni`), `unpinned_and_tampered_modules_are_refused` |
| вредоносное обновление зависимости | точные версии, `Cargo.lock` закоммичен и используется с `--locked`, `cargo deny` (advisories, sources), `tools/check-lockfile.sh` (только crates.io, у каждого пакета SHA-256) | ◐ CI проверяет; независимой сверки зависимостей (например, второй раннер) нет |
| C-код через транзитивную зависимость | `deny.toml` (`cc`, `cmake`, `bindgen`, `*-sys`), `check-unsafe.sh` | ✅ |
| подмена GitHub Actions | действия закреплены полными SHA коммитов | ✅ |
| подмена собранного гостя между сборкой и приложением | `ModulePin` в приложении; CI проверяет отсутствие секции импортов и печатает SHA-256 | ◐ две сборки из разных каталогов дают один хэш гостя (`tools/check-reproducible.sh`, CI `reproducible`); независимые раннеры и APK не сравнивались (ARCHITECTURE §7, D-2) |

### 4.9 Побочные каналы

| угроза | защита | статус |
|---|---|---|
| timing-атаки из документа | у гостя нет таймера, часов и случайности; топливо — единственная мера времени, и оно детерминировано | ✅ |
| спекулятивное исполнение | интерпретатор, а не JIT; отдельный экземпляр на документ | ◐ вне строгой модели |

### 4.10 Известные ограничения и остаточные риски

Здесь перечислено то, что **не** закрыто на момент 0.2.2 (список обновлён после раунда проверки). Список честнее, чем
значки ✅ выше.

| ограничение | почему | что дальше |
|---|---|---|
| очень большие и плотно сжатые таблицы могут не уложиться в бюджет топлива/памяти и получить ошибку «слишком сложно» вместо отчёта | интерпретатор `wasmi` примерно на порядок медленнее нативного кода; события XML стоят ≈ 4 700 – 6 000 топлива (проверка, не стоимость `quick-xml` как таковая: профиль нативной сборки размазан по UTF-8, `quick-xml`, проверкам и хэшам). Замеры: ≈ 1 060 топлива на байт XML при проверке, ≈ 700 при просмотре, ≈ 4 700 при стерилизации. **Просмотр и стерильная копия получают бюджет по размеру распакованного**, который назвала проверка (`bytes_inflated`; топливо ×1 500 / ×6 000 и память ×2 / ×4 на байт, не выше потолков топлива и памяти устройства, часы не меняются): книга в 3 МБ, распакованная в 44 МБ, проверялась за 8 с, просматривалась за 8 с, стерилизовалась за 32 с на разработческой машине | сама проверка (первый проход) сохраняет бюджет 32 000 на байт входа и годна до сжатия ≈ 30:1; ускорение разбора XML внутри интерпретатора не сделано (нет профилировщика на уровне Wasm) |
| стерильная копия не для всех форматов одинакова (F-1) | на месте вырезается только OOXML; ODF и PDF пересобираются (§4.12–4.13), DOC/XLS/PPT и RTF — пересборка в новый OOXML по просмотру, с потерей вёрстки (§4.14); картинка — новый PNG; для остальных `SanitizeError::Unsupported`, кнопка не показывается | у пересборки по просмотру нет «исходного оформления» и не все элементы переносятся; сверено на файлах набора, не на документах из настоящих Word/Excel/PowerPoint |
| Access (`.mdb`/`.accdb`): рентген решает по **системному каталогу** `MSysObjects` (макрос типа `-32766` по имени `AutoExec`, модуль `-32761`, объект `VBAProject`, таблица `MSysAccessStorage`); просмотр читает таблицы, схему, связи и определения запросов | база, где эти слова лежат просто данными, находки не получает. **Побайтовый поиск** тех же имён (ASCII и UTF-16LE) включается только когда каталог не прочитан целиком: файл повреждён или обрезан (в том числе обрывается посреди страницы), каталог не открылся или чтение остановил предел. Каталог не прочитался — `Unverified`. Запросы восстанавливаются в SQL-текст по атрибутам `MSysQueries` (раскладка взята из описания Jackcess) с пометкой «не проверено» и никогда не выполняются; то, что не укладывается в известную раскладку, показывается как сохранённые строки. Страница «связей» рисует только имена таблиц и столбцов и линии `MSysRelationships`: ни одна строка данных для неё не читается |
| бинарный Visio (`.vsd`) читается как текст фигур; соединители, данные фигур, гиперссылки и мастера **не читаются** | разметка этих кусков не проверена на настоящих файлах (в 0.2.0 они выводились по догадке и могли придумывать связи); теперь они считаются и называются в уведомлении «не поддержано» | вернуть можно только вместе с настоящими `.vsd` для проверки; геометрии и 3D нет ни в VSD, ни в VSDX |
| парсеры вне песочницы | хост читает центральный каталог ZIP (подсказка бюджета, имена частей стерильной копии) и первые байты картинки; ошибка в них — ошибка на стороне приложения (память Rust, `forbid(unsafe_code)`), а не гостя | оба места ограничены (§4.21); эти два разбора остаются на хосте |
| клиент iOS (`app/`, `crates/arca-ffi`, `tools/ios`, `ios.yml`) | безопасный слой `api.rs` покрыт модульными тестами в составе workspace, сборка Swift и XCFramework автоматизирована в CI на macOS-14 | спецификация и целевая архитектура описаны в `docs/IOS_ARCHITECTURE.md` |
| обнуление памяти не гарантировано | см. §4.7 | копии в аллокаторе и зависимостях, аварийное завершение процесса, память клавиатуры и виртуальной машины Java |
| `cargo-fuzz` с покрытием и реальные вредоносные образцы не использовались | каркас `fuzz/` и таблицы враждебных входов написаны, но libFuzzer запускается только в CI (workflow `fuzz`), образцов в репозитории нет по правилу | первый настоящий прогон — задача CI |
| эвристики бинарных форматов (VBA stomping, поля Word Binary) | дают и ложные срабатывания, и пропуски; бинарный Word без читаемой таблицы кусков — `Unverified` | разбор содержания в `arca-preview` их дополняет, но не заменяет |
| побитовая воспроизводимость APK между сборками не подтверждена | гость и хостовая `libarca_jni.so` из двух каталогов совпадают побайтно (`tools/check-reproducible.sh`); `Cargo.lock` закоммичен, сборка закреплена | сравнить хэш на разных раннерах/образах и собрать APK (NDK, подпись) дважды; см. ARCHITECTURE §7 |
| имена создателей (команда Pharos) и ссылка на пожертвования (Lava.top) зафиксированы | канонический хэш SHA-256 проверен tripwire-системой UPSTREAM_FINGERPRINT | ссылка вшита в раздел О программе и проверена |
| ARM-сборки Android (`arm64-v8a`, `armeabi-v7a`), iOS, MSRV и `cargo-fuzz` в среде разработки не запускались | нет таких целей и nightly; в CI эмуляторы Android — `x86_64` | задания `mobile-targets`, `msrv` в `ci.yml`, workflow `fuzz`; прогон на ARM-устройстве |
| интерфейс и функционал Android | верифицирован на физических устройствах (0.1.5), для последующих версий контролируется матрицей эмуляторов CI (API 26–36, 14 конфигураций экранов, жестовая навигация, тёмная тема, поворот) | постоянный автоматический контроль в CI |
| просмотр — чтение содержания, не вёрстка; у PDF есть приблизительный постраничный вид (§4.19) | точная вёрстка и шрифты документов требуют отдельного, гораздо большего разбора шрифтов и графики | рисование страниц для остальных форматов — отдельный этап |
| сжатый RTF в письмах MSG проверен только на данных, собранных вручную по спецификации | под рукой не было независимого кодировщика/писем из почтовых программ | прогнать письма из Outlook и сверить |

### 4.11 Просмотр и приложение Android

| угроза | защита | статус · тесты |
|---|---|---|
| ошибка в парсере просмотра (формат — тот же враждебный вход) | разбор идёт в **новой** песочнице Wasm без импортов, как и рентген; те же лимиты памяти, топлива, времени; отмена | ✅ `e2e_sandbox`, мутационные тесты каждого парсера (`ARCA_FUZZ_ITERS`) |
| враждебный текст в результате просмотра: управляющие и bidi-символы («Trojan Source»), невидимые символы (Cf, Zl/Zp, теги, U+2060–206F, селекторы вариантов, U+00AD, U+2800 и др.), теги Unicode, в том числе в адресах, названиях листов, подписях картинок и маркерах | `Sink` очищает **каждую** строку (в том числе адреса, имена листов, подписи) и считает удалённое; у адреса, который пришлось бы обрезать, обрезки нет — такой адрес отбрасывается целиком и об этом сказано (обрезанный адрес указывал бы в другое место); текст страницы PDF очищается в госте (§4.19) | ✅ тесты `sink.rs`, `invisible_characters_of_the_audit_list_are_removed`, `names_addresses_and_markers_are_single_line`, `a_link_address_that_is_too_long_is_dropped_and_said`, `bidi_and_tag_characters_do_not_reach_the_page_view` |
| взрыв размера результата (миллионы абзацев, «бесконечная» таблица) и **молчаливая обрезка** | `Sink` ограничивает число элементов, символов, строк и столбцов **по байтам, а не по числу ячеек**; любая обрезка (текст ячейки, строки таблицы, непарный `"` в CSV, скрытые столбцы, части книги и презентации, которых нет в списке) помечена «показано не всё» и счётчиком; таблица, вышедшая за бюджет, обрезается по строкам, а не пропадает целиком | ✅ тесты `sink.rs`, `preview_of_a_large_spreadsheet_stays_inside_the_budget`, `cut_cell_text_is_reported`, `a_grid_over_the_budget_is_cut_by_rows_not_dropped`, `grid_builder_bounds_the_bytes_and_hides_columns`, `an_unclosed_csv_quote_is_reported`, `worksheet_parts_the_workbook_does_not_list_are_reported` |
| подмена текста документа текстом просмотра (строка «A → B», которую набрал автор, выдаёт себя за соединение; фигура без текста — за имя) | строки, которые вставил сам просмотр (соединения Visio, имя фигуры без текста, SQL запроса Access), несут стилевой бит `SYNTHETIC` и отличаются в интерфейсе; в просмотре нет жёстко записанных строк документа вместо уведомлений | ✅ для Visio (`vsdx.rs`, тест стиля `SYNTHETIC`); для Access SQL поддержан бит `SYNTHETIC`; отображение синтетических строк верифицировано в тестах интерфейса |
| парсер читает не тот элемент или атрибут (`x:t` чужого пространства имён принят за `w:t`, `a:Id` рядом с `r:id`, префикс связей не `r:`) | просмотр сопоставляет имена с учётом пространства имён: элемент с префиксом вне семейства офисных пространств «чужой» и пропускается, идентификатор связи берётся из атрибута в пространстве имён связей, а не из первого атрибута с таким локальным именем; закрывающий тег чужого элемента не закрывает настоящий | ✅ `a_foreign_namespace_t_is_not_word_text`, `the_end_tag_of_a_foreign_element_does_not_close_a_real_one`, `the_hyperlink_target_comes_from_the_relationship_attribute_not_a_look_alike`, `a_relationship_prefix_other_than_r_is_followed_by_its_namespace`, `the_sheet_is_found_by_the_relationship_id_attribute` |
| сокрытие: скрытый текст, листы и слайды, исправления | скрытое по умолчанию не показывается, но об этом сказано; по запросу показывается с пометкой | ✅ контрактные тесты (`android/host-test`), тесты форматов |
| ссылка в документе ведёт на внешний ресурс | у приложения нет `INTERNET`; ссылки не загружаются и сами не открываются, нажатие показывает полный адрес с кнопкой «Копировать» (у адреса с не-ASCII хостом — ещё его ASCII-вид, punycode, с предупреждением); кнопки «Открыть» для ссылок из документа нет (она есть только у собственных ссылок приложения на экране «О приложении»); системный классификатор текста для ссылок отключён | ◐ манифест без разрешений и проверка пакета — автоматически; поведение диалога и предупреждение о punycode — только код, не автоматизировано |
| вложения писем и встроенные объекты | никогда не открываются и не распаковываются, только перечисляются | ✅ тесты `msg.rs`, `ppt.rs`, `doc.rs` |
| ошибка на границе JNI (неверные дескрипторы, гонки, утечки, отмена, 32-битное переполнение сдвига) | `unsafe` в одном файле; логика — в безопасном `api.rs`; обработчики не паникуют через границу; дескрипторы — счётчики, а не указатели; размеры из Java переводятся в байты с насыщением (не оборачиваются на 32-битном `usize`) | ✅ `NativeTest` на настоящей JVM с `-Xcheck:jni`, тесты `arca-jni` (`megabyte_figures_from_the_device_never_overflow`; число проверок смотрите в журнале CI); ⚠️ 32-битный `armeabi-v7a` на устройстве не запускался |
| утечка содержимого через ОС: снимки экрана, «Недавние», автозаполнение, захват содержимого, резервные копии, логи, **диалоги** | `FLAG_SECURE` по умолчанию, скрытое превью в «Недавних» (Android 13+), автозаполнение и захват содержимого выключены; **диалог — отдельное окно**, поэтому `FLAG_SECURE`, отказ от автозаполнения и захвата применяются к каждому диалогу отдельно (`Ui.harden`); `allowBackup=false`, `data_extraction_rules.xml` исключает всё; на диск документ не пишется, содержимое не логируется | ◐ манифест и код; поведение на устройствах — не автоматизировано |
| утечка через буфер обмена (скопированный фрагмент документа остаётся доступен другим приложениям, показывается подсказкой клавиатуры) | копия помечается `android.content.extra.IS_SENSITIVE` (Android 13+), копируется только очищенный текст (`Fmt.safe`), через 60 с буфер очищается, если в нём по-прежнему наша запись (Android 9+); на старых версиях пометки нет | ◐ код; логику очистки `core-test` не покрывает, на устройстве не проверялось |
| локальные счётчики и напоминания: необязательный счётчик «Паспорта» (плоские итоги без дат, **выключен по умолчанию**) и три числа карточки поддержки (день установки, день последней карточки, позиция сообщения) в `SharedPreferences`; карточка напоминания на главном экране | разрешений нет, уведомлений, будильников и приёмников нет; `INTERNET` нет; журнал дней использования (400 дневных корзин до 0.4.5) удалён: в блобе нет ни дат, ни времени, блоб старого формата не читается, найденный при выключенном счётчике стирается при запуске; счётчик можно выключить (блоб удаляется) или обнулить; правила показа (окно 11:00–20:30, не чаще раза в 7 дней) живут в ядре и покрыты тестами | ◐ `build-apk.sh` и `arca-devtools check-resources` проверяют манифест; тесты `stats` проверяют, что в блобе нет места для даты. Остаточное: включённый счётчик — это итоговая метаданная об использовании, лежащая на устройстве; три числа карточки говорят, когда приложение поставили и когда показывали карточку |
| эксплуатация разбора документа в процессе приложения (повреждение памяти в разборщике, выход из Wasm) | документ разбирает изолированный процесс `SandboxService` (`isolatedProcess`, домен `isolated_app`, seccomp, нет данных приложения и сети), внутри — Wasm-песочница; процесс создаётся на каждый документ и убивается с ним; при отказе системы привязать сервис в течение 1,5 с документ разбирается в процессе приложения, и «Диагностика» говорит об этом прямо; бинарник собран с полным RELRO, `-z now`, неисполняемым стеком (`arca-devtools elf-check --harden`) | ◐ код, компиляция, тесты протокола на уровне Java-ядра; запуск на устройствах и эмуляторах не выполнялся: поведение `isolated_app` на конкретных прошивках не проверено |
| недоверенный источник файла и недоверенный ответ ядра | фильтры «Открыть с помощью» не `BROWSABLE` (веб-страница не запустит приложение) и принимают только `content://`; `file://` из явного интента — только обычный файл вне хранилища приложения, `/proc`, `/sys`, `/dev` (по каноническому пути); канал или сокет (`getStatSize() < 0`) отклоняются, потому что чтение из них блокируется без таймаута; ответ ядра длиннее 16 МиБ отвергается до разбора JSON; `OutOfMemoryError` в фоновой задаче — обычный экран ошибки; кэш картинок 48 МиБ, страница PDF — масштаб 8 и высота 32 768 px | ◐ код, часть логики в `core-test` и `host-test`; ограничение: провайдер, выдающий обычный файл, но тормозящий чтение, не останавливается (отменить можно только разбор) |
| подмена или повреждение приложения | пакет подписан (APK Signature Scheme v2/v3), отпечаток сертификата и SHA-256 публикуются в заметках релиза; гость вшит в библиотеку и закреплён по SHA-256; **ключ релиза доступен только заданию `package-release`** (окружение `release` репозитория, тег `v*`), передаётся шагам через их `env:`, а не через `GITHUB_ENV`; сборки pull request и `main` секретов не видят и подписываются одноразовым ключом | ✅ проверка пакета в CI (`apksigner verify`); разделение секретов — ◐ устройство workflow, правила защиты окружения `release` (обязательные проверяющие, ограничение по тегам) настраиваются в репозитории; сверка отпечатка пользователем — вручную |
| поддельная атрибуция в релизной сборке | на теге `ARCA_RELEASE=1` не собирается при заглушках | ✅ `integrity.rs`, задание `package` |

### 4.12 Стерильная копия OpenDocument

| угроза | защита | статус · тесты |
|---|---|---|
| макросы Basic/Python/JavaScript/BeanShell, диалоги, привязки интерфейса | члены `Basic/`, `Scripts/`, `Dialogs/`, `Configurations2/` не попадают в копию; `office:scripts` и `script:event-listener` удаляются из XML; записи удаляются из манифеста | ✅ `opendocument_is_rebuilt_without_anything_active_remote_or_personal` |
| ссылки, уходящие за пределы пакета или вызывающие макрос (`vnd.sun.star.script:`, `macro:`, `http:`, UNC, абсолютные пути) | гиперссылка теряет адрес и остаётся текстом; картинки, кадры, источники таблиц и шрифты с таким адресом удаляются; цель кнопки формы (`form:target-location`) удаляется | ✅ там же |
| DDE, формулы с `WEBSERVICE`/`HYPERLINK`/ссылками на другие документы | `text:dde-*`/`table:dde-*` удаляются; атрибут `table:formula` удаляется, кэшированное значение остаётся | ✅ там же |
| встроенные объекты, OLE, исполняемые файлы под безобидным именем | `Object N/`, `ObjectReplacements/`, любой член с сигнатурой OLE2 или исполняемого файла удаляется; кадр теряет `draw:object` | ✅ `encrypted_and_hostile_opendocument_are_refused_not_mangled` |
| приватность: автор, организация, время правок, принтер, миниатюра, поля «автор»/«дата изменения» | `office:meta` очищается, имя принтера из `settings.xml` и миниатюра удаляются, поля документа, показывающие эти сведения, удаляются; правки принимаются, комментарии удаляются (по настройкам) | ✅ `opendocument_options_keep_what_was_not_asked_to_go` |
| подпись, которая перестала бы совпадать | `documentsignatures.xml` и `macrosignatures.xml` удаляются | ✅ |
| зашифрованный пакет | отказ (`SanitizeError::Rebuild`), а не порча | ✅ |
| результат не стерилен | копия заново проходит рентген; макросы, внешние ссылки и непроверенные части дают ошибку проверки; на копиях настоящих ODT/ODS/ODP из LibreOffice вердикт `Clean`, LibreOffice их открывает | ✅ |

Ограничения: удаляются **все** встроенные объекты, в том числе безобидные диаграммы (их данные хранятся в `Object N/`); картинки в `Pictures/` не перекодируются (метаданные внутри самих картинок остаются); `remove_custom_xml` для ODF не имеет смысла и игнорируется.

### 4.13 Стерильная копия PDF

Копия PDF **строится заново**, а не очищается вырезанием: из исходного файла берётся только то, до чего
доходят страницы (их рамки, поворот, ресурсы, содержимое, а дальше — шрифты, рисунки, формы, цветовые
пространства), всё остальное в файл не попадает.

| угроза | защита | статус · тесты |
|---|---|---|
| JavaScript, `/OpenAction`, `/AA`, действия `Launch`, `URI`, `SubmitForm`, `ImportData`, `GoToR`/`GoToE`, `Rendition`, `Movie`, `Sound`, `RichMedia` | новый каталог содержит только `/Pages` (и настройки слоёв); ключи и словари-действия не копируются нигде, даже вглубь ресурсов | ✅ `a_hostile_pdf_is_rebuilt_from_what_the_pages_need` |
| формы (AcroForm, XFA), вложения (`/EmbeddedFiles`, `/EF`), портфолио, подписи, `/DSS`, `/Perms` | не копируются | ✅ там же |
| скрытое в старых редакциях, удалённых объектах, «хвосте» после `%%EOF`, в потоках объектов | недостижимые со страниц объекты не копируются; сколько их было — в отчёте | ✅ (инкрементальное обновление, объектные потоки — проверено на файлах pikepdf/pypdf) |
| метаданные: `/Info`, XMP, миниатюры, структура тегов | не копируются (`/Info` — по настройке) | ✅ |
| аннотации | как аннотации не копируются (ссылки и их адреса, всплывающие окна, скрытые — теряются); их **вид** (штамп, подсветка, значение поля формы, флажок) рисуется в страницу по алгоритму 8.1 ISO 32000-1 §12.5.5; комментарии рецензентов — по настройке | ✅ вид формы совпадает с оригиналом в pdfium |
| зашифрованный PDF | отказ со статусом «зашифрован», а не порча | ✅ |
| результат не стерилен | копия заново проходит рентген (вердикт `Clean`) | ✅ `a_pdf_gets_a_sterile_copy_too`, сквозной прогон гостя |

Содержимое страниц, шрифты и рисунки копируются **байт в байт, не распаковываясь**: копия не зависит от
исправности какого-либо декодера. Результат детерминирован (классическая таблица xref, фиксированный
порядок), повторная очистка очищенного файла ничего не меняет.

Проверено на восьми настоящих PDF (reportlab, LibreOffice, pypdf): число страниц и текст совпадают,
отрисовка в pdfium **пиксель-в-пиксель** (расхождение 0), `pypdf` открывает копии.

Ограничения: закладки, именованные назначения и ссылки теряются; интерактивные формы становятся
обычным содержимым (значения остаются видимыми, но не редактируются); подписи недействительны по определению;
невидимые (`Hidden`/`NoView`) аннотации не рисуются; PDF/A-метки (`OutputIntents`) и теги доступности не
сохраняются; метаданные внутри самих картинок и шрифтов не изменяются; видимый, но «белый на белом» текст
остаётся, как и в исходном.

### 4.14 Стерильная копия бинарного Office и RTF

DOC, XLS, PPT и RTF **не очищаются на месте**: в них нет безопасного способа вырезать макрос, поле или
OLE-объект, не понимая формата целиком. Вместо этого просмотрщик читает файл (тем же разбором, что и для
показа, под теми же бюджетами) и по прочитанному **пишется новый OOXML**: DOC и RTF → `.docx`, XLS →
`.xlsx`, PPT → `.pptx`. В копию может попасть только то, что есть в модели просмотра: текст и его начертание,
абзацы и списки, таблицы, значения ячеек, названия листов, слайды и заметки, описания картинок. Макросов,
OLE-объектов, полей, форм, внешних связей и метаданных в этой модели нет — взяться им в копии неоткуда.

| угроза | защита | статус · тесты |
|---|---|---|
| VBA/XLM, OLE, `\object`, поля (DDE, INCLUDETEXT…), ActiveX | не представимы в модели; пакет пишется только из фиксированных частей | ✅ `an_rtf_with_an_exploit_object_comes_out_clean`, `legacy_office_and_rtf_get_an_ooxml_copy` |
| внешние связи и адреса | ссылки в копии не пишутся как связи; адрес следует за словами ссылки в скобках (внутренние переходы `#закладка` и ссылка, совпадающая со своим текстом, не дублируются); для ячеек таблицы — тоже | ✅ `a_sterile_copy_shows_where_each_link_pointed`, `the_address_is_shown_unless_it_is_the_words_or_an_internal_jump`, `a_linked_spreadsheet_cell_keeps_its_address_in_the_copy` (в 0.2.0 адреса терялись, хотя документ обещал обратное) |
| имена листов xlsx-копии: «Лист» и «ЛИСТ» — одно имя для Excel | уникальность без учёта регистра во всех письменностях (`Лист`, `ЛИСТ (2)`, `лист (3)`) | ✅ `sheet_names_are_unique_in_every_script` |
| инъекция в XML (`<`, `&`, управляющие символы, имена листов) | всё экранируется, недопустимые символы XML заменяются; имена листов приводятся к правилам Excel | ✅ |
| усечённый разбор | копия из обрезанного просмотра **отвергается**, а не выдаётся как «полная» | ✅ `other_formats_and_broken_files_are_refused` |
| битые и мутированные файлы | ошибка, а не паника | ✅ `mutated_legacy_files_never_panic` |
| результат не стерилен | копия заново проходит рентген (`Clean`) | ✅ тест гостя, сквозной прогон `ContractTest` |

Ревизии и комментарии: по настройкам — принять правки (удалённое не попадает в копию), убрать комментарии.
Результат детерминирован.

Ограничения: это **пересборка по просмотру**, а не конвертер. Раскладка упрощается (колонки, плавающие
фигуры, диаграммы, рисунки как картинки не переносятся — на их месте подпись), формулы XLS остаются
сохранённым результатом, у слайдов нет исходного оформления; копия меняет формат (имя файла получает новое
расширение). Если важна точная вёрстка — используйте PDF-копию. Сверено только на файлах из тестового набора
(LibreOffice и вручную собранных), а не на документах из реальных Word/Excel/PowerPoint.

### 4.15 Вычисление формул

Формулы XLSX без сохранённого результата (и файлов с `fullCalcOnLoad`) считает `arca-preview::formula`
— внутри песочницы, на тех же ограничениях, что и разбор. Это не табличный процессор: считать можно
только значения этой же книги.

| угроза | защита | статус · тесты |
|---|---|---|
| формула, которая ходит наружу или меняет состояние (`WEBSERVICE`, `CALL`, `REGISTER`, `HYPERLINK`, `INDIRECT`, DDE, внешние книги `[1]Sheet!A1`, именованные диапазоны, структурные ссылки) | эти имена и конструкции не известны вычислителю: ячейка остаётся текстом формулы; список функций — закрытый (`match` без «по умолчанию, вызвать»), чтения часов и случайных чисел нет вовсе | ✅ `chains_cycles_and_unknowns_are_left_alone` |
| «бомбы»: `A1=A1`, цепочки в сотни тысяч ячеек, вложенность, огромные диапазоны (`SUM(A:XFD)`), `REPT("x",1E9)` | длина формулы 4096 знаков, ≤ 2048 токенов, вложенность ≤ 48, цепочка зависимостей ≤ 96 (глубина рекурсии), бюджет 150 000 шагов (каждая посещённая ячейка диапазона — шаг), текст ≤ 32 767 знаков, ≤ 10 000 вычисляемых формул на книгу, ≤ 150 000 запомненных ячеек; циклы — «не вычислено» | ✅ `budgets_hold`, `random_formulas_never_panic` |
| подмена правды: пользователь принимает вычисленное за сохранённое Excel | вычисленное — отдельный вид ячейки (`Calculated`, курсив), в отчёте счётчик «вычислено Arca»; сохранённый результат не пересчитывается, кроме файлов, сами просящих пересчёт | ✅ `formulas_without_a_stored_result_are_calculated_and_marked` |
| расхождение с Excel (тихо неверное число) | всё, что вычислитель не берётся повторить так же, как Excel, — «не вычислено»: массивы и неявное пересечение, даты как текст, `SEARCH` с подстановками, 1900-й год до 1 марта, циклические ссылки | ✅ |
| порядок и память | вычисление по ссылкам, итеративные проходы вперёд и назад; результаты не хранятся дольше проверки | ✅ |

Сверка с LibreOffice (внешний оракул, `tools/fixtures/make_formulas.py` и случайные формулы): на ≈ 10 000
случайных формул из ≈ 40 функций расхождения были только там, где LibreOffice отличается от Excel
(логические значения в `COUNT`/`SUMIF`, пустая строка в `COUNTA`, трёхзначная степень в `E+040`, текст-число в
`SUM("5")`) — Arca следует Excel. С Excel как таковым сверки не было.

Ограничения: считаются только XLSX; XLS и ODS показывают сохранённое; около 80 функций; ссылки на другие листы
работают, на другие книги — нет; `TEXT` использует тот же разбор форматов, что и показ ячеек (без локалей).
Цена: ≈ 120 – 200 тыс. единиц топлива на формулу и второй проход по листам (только для книг, где есть что
считать).

### 4.16 Зашифрованные документы

С ABI v3 читатель может передать пароль. Расшифровка — в госте (`arca-preview::decrypt`, примитивы
`arca-container::crypto`: MD5, SHA-1/2, AES, RC4 — свои, без `unsafe`), результат — обычная копия, которая идёт
через тот же X-ray и разбор.

| угроза | защита | статус · тесты |
|---|---|---|
| утечка пароля или ключа | пароль — в префиксе запроса, исчезает вместе с памятью гостя (она стирается); хост держит его в `Zeroizing` (в том числе промежуточные копии префикса запроса и буфер чтения документа), Java — в `char[]`, затираемом после вызова; ключи AES/RC4 в `arca-container` и ключ PDF обнуляются при `Drop`, их `Debug` ничего не показывает; ни в отчёте, ни в логе, ни на диске | ✅ `a_password_travels_in_front_of_the_document`, `ContractTest.passwordChecks`, `debug_output_of_the_host_shows_neither_documents_nor_passwords`; ◐ стирание ключей наблюдаемым тестом не подтверждено |
| «бомба» стрейчинга (`spinCount=4e9`) | потолок 100 000 итераций (то, что пишет Office) — выше отказ `TooExpensive`; общий потолок топлива остаётся | ✅ `damaged_encryption_data_is_refused_not_trusted` (spinCount 999999) |
| повреждённые данные шифрования | любая длина/смещение проверяются; ошибка — `Malformed`, не паника; случайные искажения (fuzz) не роняют | ✅ `damaged_encryption_data_is_refused_not_trusted`, `plain_files_are_not_encrypted_and_junk_is_not_a_panic` |
| подмена зашифрованного пакета Office (целостность) | у agile-пакета проверяется HMAC (`dataIntegrity`, MS-OFFCRYPTO 2.3.4.14) над всем пакетом; несовпадение — отказ; неизвестный метод фильтра шифрования PDF — ошибка, а не «читать как открытый текст»; CBC-расшифровка требует целых блоков, снятие PKCS#7 не различает причин отказа | ✅ `hmac_matches_the_published_test_vectors`, `damaged_encryption_data_is_refused_not_trusted`; ⚠️ настоящими Word/Acrobat файлами не проверялось |
| неверный пароль принят за верный | проверка по верификатору (Office) и по `U`/`UE` (PDF); при ошибке — статус `WrongPassword`, а не мусор | ✅ `a_wrong_password_is_recognised_and_nothing_leaks`, `wrong_passwords_are_refused` |
| расшифрованное содержимое активно | не доверяется: копия проходит тот же X-ray, вердикт «Чистый» только если всё проверено; макросы в зашифрованном файле находятся | ✅ `ContractTest.passwordChecks` (`enc-*` через настоящий движок) |
| подмена «открылось без пароля» | PDF, закрытый лишь от правки, открывается пустым паролем и **отмечается** (`OK_UNLOCKED`); пароль не нужен, но и не подменяется | ✅ `a_file_with_an_empty_user_password_opens_by_itself` |
| Расхождение с реальными программами | сверка с `msoffcrypto-tool`, `pikepdf` (qpdf), `cryptography`, `hashlib`; стандартное шифрование Office написано нами по спецификации и сверено чтением `msoffcrypto`; настоящих Word/Acrobat файлов не было | ⚠️ |

Не поддерживается: RC4 в DOC/XLS/PPT, зашифрованный ODF, PDF с нестандартным обработчиком; R5 (черновая) не проверена.
Стерильная копия зашифрованного файла — копия **расшифрованного** содержимого, уже без пароля; интерфейс отдельно об этом не предупреждает.
Цена: Agile ≈ 1,7·10⁹ единиц топлива (100 000 SHA-512), стандартное ≈ 0,65·10⁹, PDF AES-256 ≈ 0,7·10⁹.

### 4.17 Просмотр Access

`arca-preview::access` читает базы Jet 3 / Jet 4 / ACE как набор страниц внутри песочницы (сам формат читает `arca_container::jet`, общий с рентгеном). Ничего не исполняется: формы, отчёты, макросы и модули только перечисляются по каталогу `MSysObjects` (содержимое не читается); определения запросов читаются как **данные** (строки `MSysQueries`) и показываются, но не выполняются.
запросы, формы, отчёты, макросы и модули только перечисляются по каталогу `MSysObjects`.

| угроза | защита | статус · тесты |
|---|---|---|
| SQL запроса, макрос `AutoExec`, VBA, действие, которое срабатывает при открытии | содержимое макросов, модулей, форм и отчётов не читается вовсе — только имя и тип; запрос показывается строками, как их хранит Access, а для простого `SELECT` (без соединений, удалённой базы, неизвестных атрибутов) — ещё и приближённым SQL с пометкой `SYNTHETIC` и уведомлением «не проверено»; `UPDATE`/`DELETE`/`INSERT`/`UNION`/перекрёстные/сквозные запросы SQL не получают и подписаны своим видом, ничего не выдумывается (в 0.2.0 любой запрос выводился как `SELECT`); число строк запроса и их суммарный объём ограничены; из таблиц берутся значения, не выражения | ✅ `tables_become_sheets_and_objects_are_listed`; запросы — ◐ тесты `access/tests.rs`, но **раскладка атрибутов `MSysQueries` не сверена с запросами настоящих баз** (в тестовых базах запросов нет): SQL может быть неверным, поэтому он помечен |
| связанные таблицы (путь к чужому файлу, ODBC-строка с паролем) | перечисляются по имени; база, на которую они указывают, не открывается, строка подключения не показывается | ✅ |
| гиперссылка в поле | текст виден, адрес показан как адрес и не открывается | ✅ `values_are_shown_the_way_access_shows_them` |
| OLE-объекты, вложения, двоичные поля | не читаются и не раскрываются: показывается размер | ✅ |
| повреждённые страницы, циклы в цепочках (переполнение строк, длинные значения, продолжение определения таблицы), огромные счётчики | цепочки ограничены (8 переходов переполнения, 64 страницы определения, число страниц файла для длинного значения), строк просматривается ≤ 4 000 000 на таблицу, значение ≤ 4 МиБ, все смещения проверяются; случайные и структурные искажения не роняют разбор | ✅ `damaged_databases_are_errors_or_partial_never_panics` |
| шифрованная или защищённая паролем база принята за обычную | ключ шифрования и пароль в заголовке проверяются; такая база не открывается (`Encrypted`), вердикт остаётся `Unverified` — пароль Access хранится в самой базе и проверяется читателем, Arca мимо него не смотрит | ✅ `encrypted_and_password_protected_databases_are_reported_not_opened` |
| «удалённое» осталось в файле | Access не стирает удалённые записи до сжатия; их число показывается (`DeletedRecords`), сами они — нет | ✅ |
| тихо неверные значения | сверка с Jackcess (независимая реализация) по типизированному дампу всех таблиц: 12 настоящих файлов из его тестов (Access 97, 2000, 2003, 2007, 2010) и две базы, созданные им же (2000, 2016) с каждым типом, длинным текстом, OLE на нескольких страницах, удалёнными строками | ✅ `every_database_reads_like_jackcess` |

Не делается: исполнение запросов; верный SQL для всего, кроме простого `SELECT`; индексы, связи и правила; шифрованные базы Access 2007+ (CNG) и Jet-шифрование страниц; проверка файлами, написанными самим Microsoft Access (только Jackcess и его тестовые файлы). Текст рентгена по Access — побайтовый поиск плюс каталог (§4.10): `VBAProject` и `AutoExec` находятся и в безобидных базах, а нечитаемый каталог даёт `Unverified`.
файлами, написанными самим Microsoft Access (только Jackcess и его тестовые файлы); текст рентгена по Access остаётся
поиском строк (F-2), так что `VBAProject` и `AutoExec` находятся и в безобидных базах.

### 4.18 Просмотр бинарного Visio

`arca-preview::vsd` читает поток `VisioDocument` (дерево указателей на потоки; часть потоков сжата вариантом LZ77 с окном 4096) и берёт из
страниц текст фигур. Мастера, геометрия, соединители, данные фигур и гиперссылки не читаются, ничего не исполняется.

| угроза | защита | статус · тесты |
|---|---|---|
| «бомба» сжатия | суммарный выход распаковки ограничен общим бюджетом просмотра (`inflate_budget`); один код даёт ≤ 18 байт, так что рост ограничен и без него | ✅ `decompression_is_bounded_and_survives_truncation` |
| циклы и взрыв дерева указателей (контейнер, указывающий на себя; счётчик 2³²) | открывается не более 200 000 потоков, не более 100 000 указателей в контейнере; читаются только потоки страниц и таблица имён (мастера не открываются вовсе) | ✅ `hostile_structures_stay_bounded` |
| повреждённые или обрезанные куски, цепочки групп, отрицательные и огромные размеры, позиции вне диапазона целых | все смещения и длины проверяются; глубина групп ≤ 64, форм на страницу ≤ 100 000, фигура показывается один раз (список групп, ссылающийся на себя, не зацикливает); ключ порядка чтения не переполняется (так найдена и исправлена та же ошибка в `vsdx`) | ✅ `damaged_drawings_never_panic`, `positions_beyond_the_range_of_integers_do_not_overflow_the_sort` |
| поддельный текст (управляющие и невидимые символы) | управляющие символы убираются при чтении, остальное — через общий `Sink` | ✅ |
| придуманные связи и подписи (в 0.2.0 соединения читались перекрывающимися парами слов с шагом 4, гиперссылки — по эвристике, подпись «внешняя ссылка» ставилась на каждую строку куска `0xc4`) | соединители, данные фигур, гиперссылки и мастера не читаются и **не выводятся**: их разметка не проверена на настоящих файлах; число и вид пропущенного называются в уведомлении «не поддержано»; арифметика размеров — с проверкой (`size + 4` в 32-битном госте), индексация — через `get` | ✅ `vsd/tests.rs` (структура собирается тестами; настоящие `.vsd` в репозитории не лежат); ⚠️ сверка с libvisio — только по тексту трёх настоящих файлов (предыдущая строка) |
| тихо неверный порядок или имя страницы | текст сверен с тем, что читает libvisio (LibreOffice) на трёх настоящих файлах: Visio 2000 и две 2003; имена страниц берутся из таблицы имён по номеру — для многостраничных файлов это предположение, не проверенное на образцах | ⚠️ `real_files_read_like_libvisio` (файлы из набора Apache POI в репозиторий не добавлены) |

Не делается: Visio 5 и старше, шифрование (его в этом формате нет), геометрия, 3D, чтение мастеров, соединителей, данных фигур и гиперссылок. Visio OOXML (`.vsdx`) показывает текст фигур, данные фигур, ссылки и соединения; соединения и имена фигур без текста помечены `SYNTHETIC`; 3D там не реализован (заметки 0.2.0 утверждали обратное).

### 4.19 Постраничный вид (PDF, слайды, листы, Visio, схема связей Access)

Операция ABI v4 `Render` рисует страницу PDF, слайд (PPTX/ODP), лист таблицы (XLSX/ODS/XLS), страницу схемы Visio (VSDX/VSD) или схему связей базы Access (MDB/ACCDB): тот же разбор, что и у просмотра текста, но результат — не текст, а
список примитивов (контуры, строки текста, отметки вместо картинок, обрезки) в точках, с началом в левом верхнем углу и с учётом поворота
страницы. Приложение рисует его на `Canvas`. Картинки и шрифты **не** декодируются и не загружаются: картинка — серый прямоугольник с
подписью, шрифт заменяется системным (гарнитура и стиль берутся из имени шрифта), градиенты и узоры — плоским цветом (о приближении
сказано в уведомлении). Скрытый текст и слои, выключенные в документе, не рисуются.

| угроза | защита | статус · тесты |
|---|---|---|
| рисунок как канал исполнения (шрифты, JBIG2/JPX и прочие декодеры картинок в приложении) | в приложение приходят только числа, цвета и строки; ни шрифтов, ни растров из документа в нём нет, `Canvas` получает контуры и текст | ✅ `pictures_become_placeholders_with_their_matrix` |
| текст страницы PDF (`Prim::Text`, подписи картинок) обходил очистку `Sink` и мог нести bidi-управляющие и невидимые символы в рисуемую строку | текст и подписи страницы очищаются в госте (`clean_page_text`), страница при этом помечается приблизительной; рисует текст системный шрифт `Canvas` (гарнитура по имени шрифта), шрифты документа не загружаются; вызовов `Fmt.safe` для текста страницы в `PageDrawing`/`PageView` нет (поиск по исходникам), то есть вся очистка — в госте | ✅ `bidi_and_tag_characters_do_not_reach_the_page_view`, `a_text_map_that_disagrees_with_the_encoding_is_reported`, `a_text_map_with_huge_destinations_maps_nothing`; рисование на устройстве — ⚠️ |
| взрыв модели (миллионы контуров, огромные пути) | не более 150 000 примитивов и 200 000 чисел в контурах на странице; сверх этого страница обрезается и это отмечено (`truncated`); Java-разбор отвергает больше 200 000 примитивов | ✅ `hostile_content_stays_bounded_and_never_panics`, `pageDrawing` (CoreTest) |
| несуществующая страница, чужой формат | `Malformed` / `Unsupported` статусом, не ловушкой гостя; страница считается с нуля, с проверкой границ | ✅ `a_page_that_does_not_exist_and_other_formats_are_refused`, `guest_page_equals_native_page` |
| дурные размеры и числа (NaN, бесконечность, страница 10⁹ точек) | размер страницы и все числа конечны и ограничены при записи и при разборе в Java | ✅ `pageDrawing` |
| расхождение гостя и хоста | модель проходит тем же проверяемым wire-кодеком, что и остальные результаты (`Wire for f32`), и в тесте равна нативной | ✅ `a_page_survives_the_wire`, `guest_page_equals_native_page` |
| ошибка в геометрии (страница вверх ногами, поворот) | четыре поворота проверены на углах страницы; образцы выведены в SVG и просмотрены в Chromium | ✅ `the_four_rotations_put_the_corners_where_a_viewer_does`; ⚠️ рисование на настоящем `Canvas` устройства не проверялось |

Не делается: растровые картинки, настоящие шрифты (встроенные не используются), градиенты, прозрачность групп, режимы наложения, текст как контур (`Tr 1/2`
рисуется как обычный), маски и затенения; слайды, листы, диаграммы и текстовые документы страниц не имеют. Системный шрифт рисует Unicode-текст документа: символы, которые гость пропустил бы как «безвредные», рисуются как есть, поэтому очистка в госте обязательна.

### 4.20 Картинки: просмотр и стерильная копия

Картинки (файлы PNG/JPEG/GIF/BMP и картинки внутри DOCX, PPTX, XLSX, ODF, RTF и PDF) раскодирует только
`arca-image` — безопасный Rust без `unsafe` внутри гостя. Системные декодеры Android (`BitmapFactory`,
`ImageDecoder`) байтов документа не видят: в `Bitmap` попадает массив пикселей, построенный нашим кодом.
Подробно — в [IMAGES.md](IMAGES.md).

| угроза | защита | статус · тесты |
|---|---|---|
| порча памяти или эксплойт в декодере (libpng, libjpeg, libwebp, Skia, ImageIO) | декодер — безопасный Rust в Wasm без импортов; системный декодер не используется | ✅ `guest_pictures_equal_native_pictures` |
| бомба по размеру (PNG 1×1 → 60 000×60 000, огромный GIF/JPEG) | размеры и площадь проверяются по заголовку до выделения памяти; растр уменьшается «на лету»; топливо, память и дедлайн | ✅ бомба 40 000×40 000 в сквозном тесте; `jpeg_limits_and_hostile_headers` |
| взломанный гость врёт о размере растра | хост принимает размер не более 4096 и длину буфера ровно `ширина × высота × 4` | ✅ `hostile_picture_payloads_are_refused_by_the_host_decoder` |
| утечка приватности: GPS, серийный номер, автор, миниатюра, ICC, XMP, комментарии | при показе не передаются в интерфейс; перечисляются в «Рентгене»; в стерильной копии отсутствуют | ✅ `a_photo_with_everything_in_it_becomes_a_clean_png` |
| полиглот: ZIP/PDF/исполняемый файл после конца картинки | находка `PolyglotFile`/`TrailingData`; не показывается; в копии отсутствует | ✅ `bytes_after_a_picture_are_a_polyglot_finding`, `a_png_with_text_and_trailing_data_is_written_anew` |
| данные, спрятанные в байтах, которые декодер «не объясняет» (приватные чанки PNG, тело `IEND`, хвост zlib/LZW, неизвестные расширения GIF, зазоры BMP, маркеры и лишние байты JPEG, второй блок EXIF) | учёт байт: всё, что не картинка, не структура формата и не распознанные метаданные, считается в `unaccounted` и выходит в «Рентген»; проверка на полиглот идёт по всей картинке всегда; EXIF читается без потолка в 512 записей | ✅ `png_private_chunks_are_unaccounted_whatever_they_are_called`, `jpeg_private_markers_carrying_a_zip_are_unaccounted`, `gif_extensions_nobody_reads_are_unaccounted`, `bmp_gaps_reserved_words_and_row_padding_are_unaccounted`, `unaccounted_bytes_survive_the_wire`, `a_second_exif_block_is_read_too`; чего учёт не видит — содержимое энтропийных данных JPEG и сами пиксели |
| размер и время: кадр GIF в 65535², прогрессивный JPEG с тысячами проходов, RLE в BMP с усилением, маска PDF `vec![255; w·h]` до проверки площади | размеры кадра проверяются до выделения (кадр GIF — сам по себе); проходов JPEG ≤ 200 и общий лимит блоков; RLE ограничен шириной и общим объёмом работы; режимы, которым нужен весь растр, — не больше 16 Мпкс; мягкая маска PDF выбирается лениво | ✅ `a_gif_frame_far_larger_than_its_canvas_is_refused_before_anything_is_reserved`, `a_progressive_jpeg_of_thousands_of_scans_is_refused`, `bmp_rle_runs_past_the_edge_cost_no_more_than_the_codes` |
| скрытые данные в цвете прозрачных пикселей | в копии цвет полностью прозрачных пикселей обнуляется | ✅ `colour_hidden_in_transparent_pixels_is_cleared` |
| картинка по ссылке (трекинг) | удалённые картинки не загружаются | ✅ уже так для документов |
| ошибка записи копии | копия читается обратно в строгом режиме (`decode_sterile`: CRC всех чанков, Adler-32, ни повреждений, ни хвоста, ни метаданных, ни необъяснённых байт, один кадр) и сверяется по отпечатку SHA-256 пикселей, затем проходит рентген | ✅ `a_picture_file_gets_a_sterile_copy_that_is_a_plain_png`, `guest_picture_copy_equals_native_picture_copy`, `the_sterile_check_accepts_what_the_writer_wrote_and_nothing_else`, `the_sterile_check_verifies_the_zlib_checksum_the_viewer_ignores` |
| картинка без запроса человека | в документе картинка раскодируется только по тапу (настройка автопоказа выключена по умолчанию) | Android |

Стерильная копия картинки **пишется заново** из пикселей (PNG: заголовок, данные, конец), а не очищается
вырезанием: структура оригинала не доходит до системного декодера.

Ограничения: показываются PNG, JPEG (baseline и progressive), GIF (первый кадр), BMP; WebP, TIFF, HEIC, AVIF,
JPEG 2000, SVG, EMF/WMF названы и не раскодируются; картинки внутри бинарных DOC/PPT пока не показываются.
Цветовой профиль не применяется (sRGB); CMYK преобразуется приближённо; картинка больше 4 Мпкс (16 Мпкс с опцией «полный размер»)
в копии уменьшается; фото в PNG больше, чем в JPEG. Данные, спрятанные в самих видимых пикселях
(стеганография), копия не убирает.

### 4.21 Граница хост ↔ гость: что хост проверяет сам

Гость считается скомпрометированным (§3, граница B2). Всё, что от него приходит, хост проверяет
независимо. Это защита от гостя, который врёт, а не от гостя, который ошибается тоньше, чем умеет
проверить хост.

| угроза | защита | статус · тесты |
|---|---|---|
| гость сообщает «стерильная копия проверена», подсовывая другие байты | хост пересчитывает SHA-256 копии и сверяет с отчётом гостя; несовпадение — отказ | ✅ `a_sterile_copy_must_be_the_file_the_report_describes` |
| гость забыл или оставил самое опасное (макрос, ActiveX, OLE, вложение) в ZIP-копии | хост разбирает ZIP-копию и отказывает, если имя части содержит `vbaproject.bin`, `activex`, `oleobject`, `embeddings/` или `macrosheets/` | ✅ `a_sterile_copy_that_still_holds_what_sterilisation_removes_is_refused`; ◐ это растяжка, не вторая стерилизация: копии не в формате ZIP (PDF, картинка) проверяются лишь хэшем, а тонкие ошибки стерилизатора она не ловит |
| гость присылает ответ с огромными аллокациями (wire-усиление) | общий бюджет декодера (§4.2) | ✅ тесты бюджета |
| гость присылает огромный результат | предел вывода по операции, `try_reserve_exact` (§4.2) | ✅ `only_a_sterile_copy_may_come_back_larger_than_a_report_can_be` |
| гость присылает строки с управляющими и bidi-символами | строки отчёта и просмотра выходят в JSON одной видимой строкой | ✅ `strings_the_guest_reports_reach_the_app_as_one_visible_line` |
| гость врёт о размере растра | хост принимает размер не более 4096 и длину буфера ровно `ширина × высота × 4` | ✅ `hostile_picture_payloads_are_refused_by_the_host_decoder` |
| подмена гостя при сборке приложения | ожидаемый SHA-256 вшит при сборке и сверяется в `Engine::pinned` (§4.8) | ✅ |
| **хост читает документ сам** (`sniff_inflated`: центральный каталог ZIP; `arca_image::sniff`: первые байты) | оба места — Safe Rust; размеры из каталога недоверенные (запись ≤ `сжатый × 100 + 1 МиБ` и ≤ 256 МиБ, сумма ≤ 100 × файла и 1 ГиБ, > 20 000 записей — подсказки нет); подсказка двигает только бюджет под фиксированный потолок | ✅ `declared_sizes_cannot_buy_a_big_budget`; ◐ остаточный риск: парсер вне песочницы (§4.10) |

### 4.22 Расхождения читателей и пространства имён

Классический обход: сканер читает файл одним способом, а Word, Excel или просмотрщик — другим. В 0.2.0
такие расхождения были частью поверхности атаки (роль части по подстроке имени, первый атрибут с
таким локальным именем, UTF-16 без метки, второй конец архива). Теперь правило единое: **если читатели
могут увидеть разные документы, это находка и пробел покрытия, а не молчаливый выбор одного из них.**

| расхождение | как закрыто | статус · тесты |
|---|---|---|
| роль части OOXML по имени (`word/mycomments.xml` как «комментарии», основной текст не сканируется) | роль — по типу содержимого, затем по связи, и лишь затем по имени; несогласие источников — находка; сканер и стерилизатор используют одну функцию | ✅ `b2_a_renamed_main_part_is_still_scanned`, `b2_a_part_named_like_something_else_is_a_coverage_gap_even_when_benign`, `b2_the_sanitizer_decides_by_the_same_rules` |
| один файл — два формата (OLE2 с потоками Word и Excel; ZIP с `[Content_Types].xml` и `mimetype`) | сканируются обе интерпретации; `ParserDifferential`/`PolyglotFile` — пробел покрытия | ✅ `b3_a_compound_file_that_is_two_documents_is_scanned_as_both`, `b9_an_archive_that_is_a_package_and_an_opendocument_is_scanned_as_both` |
| атрибут с тем же локальным именем под другим префиксом в рентгене (`foo:instr` рядом с `w:instr`, `a:Target` рядом с `Target`) | при разборе OOXML такое расхождение — находка; предпочитается атрибут ожидаемого пространства; ODF, где это штатно, проверку отключает | ✅ `b4_a_foreign_prefixed_instr_next_to_the_real_one_is_not_ignored`, `b4_a_prefixed_target_mode_cannot_hide_that_a_reference_is_external`, `b4_opendocument_may_repeat_a_local_name_under_two_prefixes` |
| то же в просмотре (`arca-preview`) | имена сопоставляются с учётом пространства имён; чужое пространство — «чужой» элемент; идентификатор связи берётся из атрибута в пространстве имён связей; коллизии не молчат (§4.11) | ✅ `the_hyperlink_target_comes_from_the_relationship_attribute_not_a_look_alike` и др. |
| второе имя части (Unicode Path 0x7075, локальный заголовок против каталога), подложный конец каталога, дескриптор, называющий размеры иначе | отказ на уровне контейнера (§4.6) | ✅ тесты `zip/tests.rs` |
| XML в UTF-16/32 без метки порядка байт, EBCDIC, `encoding=`, не совпадающий с байтами, символы вне `Char` XML | отказ при чтении (`XmlError::Encoding`); при переписывании UTF-16 → UTF-8 декларация исправляется | ✅ `utf16_and_utf32_without_a_byte_order_mark_are_refused`, `characters_outside_the_xml_char_production_are_refused`, `the_declared_encoding_must_match_the_bytes`, `b12_a_utf16_part_is_written_as_utf8_and_says_so` |
| PDF: словарь потока и `/Length` читаются двояко | оба прочтения показываются; нечитаемое — пробел покрытия (§4.4) | ✅ `b7_a_lying_length_shows_both_readings` |
| ссылка записана так, что классификатор и Windows её понимают по-разному (`\/host`, `%5C`) | классификация после декодирования (§4.5) | ✅ `b11_*` |
| Visio: формулы сопоставляются не так, как их разбирает Visio (пробелы, регистр) | сопоставление «как Visio» | ✅ `b10_visio_formulas_are_matched_the_way_visio_parses_them` |

Остаётся: сканер и **настоящий** Word/Excel/Acrobat — разные программы, и сверка с ними не проводилась (⚠️).
Любое новое расхождение, которое придумает автор файла, не отловится, пока о нём не знают; именно поэтому
`Clean` — утверждение «известные детекторам угрозы не найдены» (§5).

### 4.23 Изолированный процесс: что в него входит, что из него выходит, чем он укреплён

Аудит `SandboxService` и протокола `SandboxProtocol` (0.6.0). Изолированный процесс держит документ и
результаты его разбора, поэтому важны три вещи: **что по каким каналам ходит**, **что остаётся в его
памяти** и **что делает ошибка, если она случилась**.

| канал | что по нему идёт | защита | статус · тесты |
|---|---|---|---|
| Binder: вызовы ядра (`INIT`, `DOC_NEW`, `JOB_*`, `RUN`, `*_FREE`, `PING`) | числа: дескрипторы (счётчики, не указатели), коды состояния, параметры запуска | сервис не экспортируется (`exported=false`): привязаться к нему может только само приложение; привязка на один документ; приложение определяет, работает ли сервис под изолированным uid (`Isolation`), и «Диагностика» говорит об этом прямо | ◐ код и тесты протокола на Java-ядре; на устройстве не проверялось |
| Binder: документ **в** процесс | `DOC_READ_FD`: файловый дескриптор документа (читает сервис); `DOC_APPEND`: куски до 256 КиБ (запасной путь) | у процесса нет ни хранилища, ни сети, ни разрешений: дескриптор — единственное, что у него есть от файла; чужие файлы приложения ему недоступны | ◐ код; `NativeTest` (`readingFromDescriptors`) на JVM |
| Binder: пароль **в** процесс | `DOC_SET_PASSWORD`: байты пароля | буфер затирается нулями в `finally` — и при ошибке тоже; в госте пароль и ключ остаются внутри Wasm | ◐ код (затирание — только код, проверки памяти нет) |
| Binder: ответы **из** процесса | `BYTES` с выбором (`B_REPORT`, `B_PAGE`, `B_PREVIEW_*`, `B_IMAGE_*`, `B_SANITIZED_INFO`): отчёт рентгена, модель страницы, пиксели картинок, очищенный текст | всё, что выходит, — результат работы гостя над документом: очищенные строки, плоские числа, пиксели; до 64 КиБ — в ответной посылке, больше — через анонимный канал (`pipe`); клиент отбрасывает ответ длиннее `ANSWER_MAX` и ответ ядра длиннее 16 МиБ до разбора JSON | ✅ `host-test` (`ContractTest`, ~24 000 проверок на настоящих гостях и мутациях); ◐ границы размеров |
| `pipe` для больших ответов | то же, что выше | оба конца закрываются при любой ошибке (раньше при сбое записи в посылку или запуска потока оба конца утекали — исправлено в 0.6.0); читающий конец всегда закрывается у сервиса, пишущий — только если им владеет поток; байты ответа затираются нулями, когда поток их отдал (пиксели и текст документа не лежат в куче) | ◐ код (разбор путей отказа — чтением; принудительного отказа в тесте нет) |
| `SANITIZED_WRITE_FD` | дескриптор файла, куда сервис пишет стерильную копию | передача владения явная (`handOver`): если вызов не дошёл до нативной стороны (библиотеки нет, брошена ошибка), дескриптор закрывается тут же; раньше он оставался открытым в процессе, держащем документ | ◐ код; `NativeTest` покрывает путь успеха и отмену |
| логи, `/proc`, трассировка | — | содержимое документа не логируется; у `isolated_app` нет доступа к `/proc` других процессов, к `logcat` и к сети (политика SELinux платформы) | ⚠️ поведение `isolated_app` на конкретных прошивках не проверено |
| сбой процесса | — | процесс создаётся на каждый документ и умирает с ним: остатки в памяти не переживают документ; при отказе привязки в течение 1,5 с документ разбирается в процессе приложения, и «Диагностика» говорит об этом | ◐ код; на устройстве не запускалось |

**Что перенесено из практик GrapheneOS.** Для нативного кода в `AndroidManifest.xml` запрошены
`android:allowNativeHeapPointerTagging="true"` и `android:memtagMode="sync"`: на устройстве с MTE
(Pixel 8 и новее) ошибка памяти в нативной части (`libarca_jni`, интерпретатор Wasm) превращается в
немедленное завершение процесса, а не в тихую порчу данных; на устройствах без MTE запрос ничего не
делает. Уже были: полный RELRO, `-z now`, неисполняемый стек (`arca-devtools elf-check --harden`),
отсутствие разрешений, `isolatedProcess`, `FLAG_SECURE`, отказ от автозаполнения и захвата содержимого.

**Чего не сделано (и не обещается).** Не проверены: поведение `isolated_app` и `memtagMode` на живых
устройствах, утечки дескрипторов под нагрузкой (только аудит кода и тесты на хосте); обнуление буферов в
управляемой куче Java — это «лучшее из возможного» (сборщик мусора мог уже скопировать массив), а не
гарантия; нативная часть затирает ключи и пароли через `zeroize`, остальное освобождается вместе с процессом. Список ручных проверок —
в Issue «Ручная проверка на устройствах».

## 5. Вердикты и честность HUD

* `Rejected` выставляется для структурных атак (бомба, обход путей,
  XML-атака, расхождение парсеров на уровне контейнера, исполняемый файл или ярлык под видом
  документа) и для форматов, которые Arca не считает документами (HTML, пустой файл, обычный
  архив, неопознанный формат). Документ не открывается.
* `ActiveContentNeutralized` означает, что активная или критическая внешняя
  нагрузка найдена и **не будет выполнена**. Это утверждение о поведении
  Arca, а не о безвредности файла для других программ.
* `Unverified` означает, что часть файла не удалось проверить или она прочитана неоднозначно
  (зашифрована, не разобрана, превышен лимит, второй формат или второе имя части, неизвестный
  корень XML, нечитаемый каталог Access, неподдержанный фильтр PDF; полный перечень — ARCHITECTURE
  §4.4). Это не «чисто»: HUD показывает, *что именно* осталось непроверенным.
* `Clean` означает «проверено целиком, ни одно место не читалось двусмысленно, и известные детекторам
  угрозы не найдены»;
  он не гарантирует отсутствие угроз, неизвестных детекторам. Поэтому
  отображение всегда идёт через песочницу, независимо от вердикта.
* Эвристики (VBA stomping, поля бинарного Word, записи PowerPoint) могут
  давать как ложные срабатывания, так и пропуски. Прерывание разбора с `Rejected` вызывают
  структурные виды находок (`CompressionBomb`, `PathTraversal`, `ParserDifferential` на уровне
  контейнера, `XmlEntityAttack`, `DisguisedExecutable`); та же `ParserDifferential`, поднятая как
  обычная находка, делает вердикт `Unverified`.
* Вердикт описывает только то, что умеют детекторы Arca. Тесты построены на созданных командой
  враждебных входах и на безобидных документах настоящих программ (LibreOffice и др.); корпус
  реальных вредоносных образцов и документов Microsoft Office в набор не входит (⚠️).

## 6. Атрибуция как объект защиты

Угроза: распространитель модификации удаляет или подменяет ссылку
оригинальных создателей на поддержку.

* Юридическая мера: `LICENSE_ADDITIONAL_TERMS.md` (AGPLv3 §7(b)–(d)).
* Технические «растяжки»: закреплённый отпечаток уведомления в `build.rs`,
  валидация `fork_attribution.toml` при сборке, `check_upstream_prominence`
  для golden-тестов UI.
* Остаточный риск: в свободном ПО любую техническую меру можно удалить. Меры
  защищают от случайного нарушения и делают умышленное нарушение явным.
  Применимость условия о ссылках на пожертвования как «указания авторства» —
  юридический вопрос, по которому нет устоявшейся практики.

## 7. Как сообщить об уязвимости

См. `SECURITY.md`.
