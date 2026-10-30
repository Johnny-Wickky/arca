// SPDX-License-Identifier: AGPL-3.0-only
// Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.

import Foundation

/// The words for what a macro is and does, in the language of the app. The engine reports names
/// (`RunsProgram`, `Malicious`); the text lives here, as on Android in `strings_labels.xml`.
public enum MacroBehaviorText {
    /// The name of what a macro of this intent is.
    public static func title(intent: String) -> String {
        switch intent {
        case "Benign": return "Обычная автоматизация"
        case "Malicious": return "Вредоносный код"
        default: return "Подозрительное поведение"
        }
    }

    /// One sentence on what a macro of this intent is.
    public static func summary(intent: String) -> String {
        switch intent {
        case "Benign":
            return "Код работает только с содержимым самого документа. Запуска программ, файлов, сети, реестра и других компонентов в нём не найдено."
        case "Malicious":
            return "Код делает вредное: запускает программы, скачивает или записывает файлы, меняет систему или прячет полезную нагрузку."
        default:
            return "Код читает окружение, запускается сам или скрывает, что делает, но запуска программ, скачивания и записи файлов не найдено."
        }
    }

    /// What a behaviour is; a name from a newer engine is shown as it is.
    public static func text(kind: String) -> String {
        switch kind {
        case "CountsContent": return "Считает строки, столбцы, страницы, слова или абзацы этого документа"
        case "FormatsContent": return "Форматирует текст или ячейки: шрифты, цвета, форматы чисел, границы, ширину"
        case "EditsCells": return "Читает или меняет ячейки и диапазоны этой книги"
        case "EditsText": return "Читает или меняет текст этого документа"
        case "AutoRuns": return "Запускается сам при открытии, закрытии или изменении файла"
        case "ReadsEnvironment": return "Читает переменные окружения компьютера"
        case "ChecksSystemVersion": return "Спрашивает, на какой версии системы или приложения он работает"
        case "ReadsLocalPaths": return "Читает локальные папки, пути к файлам или имя пользователя"
        case "UsesComObject": return "Создаёт или получает другие компоненты системы (CreateObject, GetObject)"
        case "ObfuscatedCode": return "Собирает или кодирует текст так, что его нельзя прочитать как написано"
        case "RunsByName": return "Запускает процедуру или текст, имя которого задаётся во время работы"
        case "CallsNativeApi": return "Объявляет и вызывает функции системных библиотек"
        case "SendsKeys": return "Посылает нажатия клавиш в другие окна"
        case "ReadsRegistry": return "Читает реестр Windows"
        case "HiddenPayloadStore": return "Хранит данные там, куда читатель не смотрит (переменные и свойства документа)"
        case "ContainsAddress": return "Содержит веб-адрес или IP-адрес"
        case "MentionsProgram": return "Называет файл программы или скрипта"
        case "XlmMacro": return "Это лист макросов Excel 4.0: формулы, которые исполняются как код"
        case "NotFullyRead": return "Код обрезан по лимиту, остальное не прочитано"
        case "OpensAddress": return "Открывает адрес в браузере или отправляет данные из документа"
        case "RunsProgram": return "Запускает программу или командную оболочку"
        case "DownloadsFile": return "Скачивает файл или данные из сети"
        case "WritesFiles": return "Записывает или копирует файлы на диск"
        case "DeletesFiles": return "Удаляет файлы"
        case "WritesRegistry": return "Записывает в реестр Windows"
        case "InjectsCode": return "Выделяет исполняемую память или запускает в ней потоки (шелл-код)"
        case "ScheduledTask": return "Создаёт задание по расписанию, чтобы запуститься снова позже"
        case "SelfModifiesCode": return "Меняет собственный код или проект макросов во время работы"
        case "SourceMismatch": return "Есть скомпилированный код, но читаемый исходник пуст или не совпадает с ним (VBA stomping)"
        case "CallsNativeCode": return "Вызывает функции нативной библиотеки из листа макросов Excel 4.0"
        case "ExploitPrimitive": return "Использует приём из эксплойтов программ для чтения документов (heap spray, переполнение)"
        default: return kind
        }
    }

    /// Where it happens: the module and the function, or the module alone.
    public static func place(module: String, procedure: String) -> String {
        procedure.isEmpty
            ? "Модуль \(module) (вне функций)"
            : "Модуль \(module) → функция \(procedure)()"
    }

    /// Why looking at it is safe, and what the reading is worth.
    public static let safeHere = "Здесь это безопасно: Arca никогда не запускает этот код. Он показан текстом, и ничто в нём не может сработать, пока вы смотрите файл."
    public static let readingNote = "Это разбор текста кода, а не гарантия: код можно написать так, чтобы он выглядел безобидно. Не включайте макросы в Word и Excel, если не доверяете отправителю."
}
