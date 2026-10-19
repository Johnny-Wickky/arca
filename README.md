# arca
Arca — Параноидально безопасный FOSS-пакет для iOS и Android. Просмотр Office, Visio 3D и Access в изолированной Wasm-песочнице на ядре Rust. Эстетика уровня Apple, zero-trust и нулевая телеметрия.

Каждый файл считается враждебным эксплойтом: он разбирается memory-safe
кодом на Rust внутри одноразовой WebAssembly-песочницы без доступа к сети,
файлам, часам и IPC.

*A paranoid, 100% FOSS document viewer for Android and iOS. Every file is
parsed by safe Rust inside a single-use, zero-import WebAssembly sandbox.*

## Статус

| фаза | содержание | состояние |
|---|---|---|
| 1 | архитектура, модель угроз, лицензирование, линты | ✅ |
| 2 | `arca-sandbox`, `arca-xray` (рентген + стерилизатор OOXML), `arca-attribution` | ✅ (известные ограничения: [модель угроз §4.10](docs/SECURITY_THREAT_MODEL.md)) |
| 3 | `ArcaRenderTree`, парсеры OOXML / OLE2 / Visio / Access | запланировано |
| 4 | Flutter-клиент, темы, Threat X-Ray HUD, «Паспорт Хранителя» | запланировано |
| 5 | расширенный adversarial- и fuzz-набор | частично: структурный мутационный фаззер и каркас `cargo-fuzz` для ядра есть; парсеры Фазы 3 — запланировано |

## Что уже умеет ядро

- Определяет формат только по сигнатурам и отвергает zip-бомбы, Zip Slip,
  XXE, Billion Laughs, глубокую вложенность, полиглоты, расхождения парсеров
  и замаскированные исполняемые файлы.
- Находит и показывает как текст, ничего не исполняя: VBA (с исходниками),
  XLM / Excel 4.0, DDE (в том числе обфусцированный), OLE / ActiveX / пакеты,
  эксплойты Equation Editor, JavaScript и Launch в PDF, удалённые шаблоны,
  трекинг-пиксели, UNC-пути (утечки NTLM), `ms-msdt:` и т. п.
- Показывает криминалистические метаданные: автор, организация, компьютер,
  локальные пути, принтер, время правки, ревизии, комментарии.
- **Стерильная копия в один тап** (OOXML): без макросов, внешних связей,
  ревизий, комментариев и метаданных, с повторной проверкой результата.
- Не говорит «Чистый» о том, чего не проверил: зашифрованная или не
  разобранная часть даёт вердикт `Unverified`, а наводнение безобидными
  находками не может спрятать решающую.
- Исполняет всё это внутри песочницы `wasmi` с лимитами памяти (пропорциональна
  файлу, около 96 МиБ для небольшого) и CPU-топлива; топливо выдаётся порциями,
  поэтому разбор можно отменить за миллисекунды и он ограничен по времени
  (`CancelToken`, дедлайн). Модуль-гость закреплён по SHA-256 и не имеет ни
  одного импорта.

## Сборка

```sh
cargo test --workspace
cargo build -p arca-guest-xray --target wasm32-unknown-unknown --profile guest
# сквозные тесты гостя в песочнице (в release: интерпретатор в разы быстрее)
ARCA_GUEST_XRAY_WASM=target/wasm32-unknown-unknown/guest/arca_guest_xray.wasm \
    cargo test --release -p arca-guest-xray --test e2e_sandbox
cargo run -p arca-xray --example xray -- scan file.docx
# фаззинг: ARCA_FUZZ_ITERS=30000 cargo test --release -p arca-xray mutation_fuzz,
# libFuzzer — см. fuzz/README.md
```

Подробности: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md),
модель угроз: [`docs/SECURITY_THREAT_MODEL.md`](docs/SECURITY_THREAT_MODEL.md),
сообщить об уязвимости: [`SECURITY.md`](SECURITY.md).

## Поддержка проекта

В Arca нет платных функций, подписок, рекламы и трекеров. Если
программа вам полезна, поддержите её создателей — ссылки на экране
«О проекте и Поддержка» и в [`NOTICE.md`](NOTICE.md).

## Лицензия

GNU AGPL v3 ([`LICENSE`](LICENSE)) с дополнительными условиями по её
разделу 7 ([`LICENSE_ADDITIONAL_TERMS.md`](LICENSE_ADDITIONAL_TERMS.md)):
модифицированные версии сохраняют указание оригинальных создателей и их
ссылку на поддержку и вправе добавить рядом свою (`fork_attribution.toml`).
