# Security policy · Политика безопасности

## Reporting a vulnerability

Please report vulnerabilities **privately** through GitHub's
"Report a vulnerability" button (Security tab → Advisories) of the original
repository. Do not open public issues for security problems.

Useful reports include: the affected crate and commit, a minimal input file
(or a generator for it), what you observed (crash, hang, wrong verdict,
sandbox escape, attribution bypass) and what you expected.

Particularly valuable:

- any way for a document to cause network, file-system or IPC access;
- any input that makes the X-ray report `Clean` while active or remote content
  is present, or makes the sanitized copy keep it;
- panics, hangs or unbounded memory use in `arca-container` / `arca-xray`;
- escapes from or resource-limit bypasses in `arca-sandbox`;
- anything that makes the host believe a guest result it should not (a sterile copy
  that passes the host's digest and part-name checks but still carries active content,
  a decoder budget bypass in `arca-abi`), and parser differentials between the X-ray and
  the viewer or an office application.

Known limits are listed in [`docs/SECURITY_THREAT_MODEL.md`](docs/SECURITY_THREAT_MODEL.md)
section 4.10; a report on something listed there is still welcome, but please say what is new.

Please do not attach live malware without prior agreement; a description or
a defanged sample is enough to start.

## Сообщение об уязвимости

Сообщайте об уязвимостях **приватно** — кнопкой «Report a vulnerability»
(вкладка Security → Advisories) в оригинальном репозитории. Не открывайте
публичные issues по вопросам безопасности.

Особенно ценны: любой способ заставить документ обратиться к сети, файловой
системе или IPC; вход, при котором рентген показывает «Чистый» при наличии
активного или внешнего содержимого или стерильная копия его сохраняет; паники,
зависания и неограниченный расход памяти в `arca-container` и `arca-xray`;
выход из песочницы или обход её лимитов; обман хоста (стерильная копия, которая проходит
проверку хэша и имён частей, но несёт активное содержимое; обход бюджета декодера в
`arca-abi`); расхождения между рентгеном и просмотром или офисной программой.
Известные ограничения перечислены в [§4.10 модели угроз](docs/SECURITY_THREAT_MODEL.md);
сообщение о них тоже полезно, но укажите, что в нём нового.

## Supported versions

Until 1.0, only the latest commit on `main` receives security fixes. The commit tagged
`v0.2.0` did not compile and is not supported (see `docs/releases/v0.2.0.md`); use 0.2.2 or newer.

До 1.0 исправления получает только последний коммит `main`. Коммит с тегом `v0.2.0` не
компилировался и не поддерживается; используйте 0.2.2 и новее.

Threat model: [`docs/SECURITY_THREAT_MODEL.md`](docs/SECURITY_THREAT_MODEL.md).
