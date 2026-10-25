# Arca — клиент iOS (ArcaKit & ArcaApp)

> **Статус.** Нативный клиент на Swift/SwiftUI (`ArcaKit`). Сборка XCFramework и прогон тестов
> выполняются в окружении macOS (Xcode 15+, Swift 5.9+) через `tools/ios/check-ios.sh` и GitHub Actions (`ios.yml`).

> **Android-клиент готов и живёт в [`android/`](../android/README.md)**: нативное приложение на
> Java без AndroidX и Gradle поверх JNI-моста `crates/arca-jni` (см.
> [`docs/ANDROID.md`](../docs/ANDROID.md)).
> **iOS-клиент построен на нативном Swift + SwiftUI поверх C ABI (`crates/arca-ffi`)**:
> аналогично Android-части, выбор в пользу нативного Swift исключает сторонние тяжёлые фреймворки
> (Flutter), гарантируя минимальную базу доверия (Trusted Computing Base) и кристально чистую
> цепочку поставок (Zero Third-Party Supply Chain).

## Стек iOS

- **Swift 5.9+ / Swift Concurrency (`actor`, `async/await`)**
- **SwiftUI** с поддержкой ProMotion 120 Гц и динамических пружинных анимаций
- **C ABI (`crates/arca-ffi`)** через Clang-модуль `ArcaCore` (`include/arca.h`).
  Байты документа передаются через дескриптор или память напрямую в `arca-engine`,
  а в Swift возвращаются строго валидированные типобезопасные структуры и постраничные JSON-пакеты.

## Обязательные свойства клиента

1. **Ни одного сетевого разрешения.** На iOS не подключаются сетевые фреймворки.
   Ссылки на поддержку открываются системным браузером (`UIApplication.open`), только после явного нажатия.
2. **Никакой телеметрии**: нет SDK аналитики, крэш-репортинга, рекламы, удалённой конфигурации.
3. **Документы — только через системный picker и «Поделиться»**: без
   широкого доступа к файловой системе. Каждый документ открывается через
   песочницу, независимо от вердикта.
4. **Экран «О проекте и Поддержка» и «Паспорт Хранителя»** строятся только из
   `arca_about` / `arca_passport`. Карточка «Создатели оригинального ядра Arca (Поддержать)» всегда первая.
5. **Статистика «Паспорта»** хранится локально, в зашифрованном хранилище
   приложения (`PatronStore` с аппаратным шифрованием `.completeFileProtection`).
6. **Оригинальная иконография**: векторные геометрические глифы, никаких SF Symbols и Material Icons.

## Структура пакета (`app/`)

```text
app/
├── Package.swift                    # Swift Package Manager манифест
├── README.md                        # этот файл
├── Sources/
│   ├── ArcaCore/                    # C ABI интерфейс
│   │   ├── arca_core.c
│   │   └── include/
│   │       ├── arca.h               # C-заголовок ядра
│   │       └── module.modulemap     # Модуль Clang для Swift (import ArcaCore)
│   └── ArcaKit/                     # Нативный Swift SDK
│       ├── ArcaEngine.swift         # Актор-обёртка (async/await, handles, zeroize)
│       ├── ArcaError.swift          # Типобезопасные ошибки (ArcaStatus)
│       ├── Models/
│       │   ├── Report.swift         # Вердикты, угрозы, макросы, внешние ссылки
│       │   ├── Preview.swift        # Абзацы, таблицы, сетки, стили, метаданные
│       │   ├── Sanitized.swift      # Очищенная копия и повторная верификация
│       │   ├── Patron.swift         # Атрибуция, Паспорт Хранителя
│       │   └── Diagnostics.swift    # Диагностика выполнения, селф-тест, факты об изоляции
│       └── Security/
│           └── PatronStore.swift    # Локальное хранилище статистики
└── Tests/
    └── ArcaKitTests/
        └── ArcaKitTests.swift       # Тесты моделей, декодирования и инвариантов безопасности
```
