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
- escapes from or resource-limit bypasses in `arca-sandbox`.

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
выход из песочницы или обход её лимитов.

## Supported versions

Until 1.0, only the latest commit on `main` receives security fixes.

Threat model: [`docs/SECURITY_THREAT_MODEL.md`](docs/SECURITY_THREAT_MODEL.md).
