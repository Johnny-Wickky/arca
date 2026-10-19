# Фаззинг Arca

Два уровня, оба детерминированно воспроизводимы:

1. **Структурный мутационный тест** — часть обычного `cargo test`
   (`crates/arca-xray/src/tests/mutation.rs`). Мутирует все файлы корпуса:
   байты, поля заголовков ZIP/CFB граничными значениями, распакованное
   содержимое XML-частей (DOCTYPE, сущности, вложенность, огромные атрибуты) с
   перепаковкой. Проверяет: нет паник, инварианты вердикта, wire-раундтрип,
   детерминизм сканера, контракт и идемпотентность стерилизатора.

   ```sh
   ARCA_FUZZ_ITERS=30000 ARCA_FUZZ_SEED=1 \
       cargo test --release -p arca-xray mutation_fuzz
   ```

   Упавший вход сохраняется в `$TMPDIR/arca-fuzz-failure-<sha>.bin`, путь
   печатается в сообщении. Раз в неделю то же с большим числом итераций и новым
   сидом запускает workflow `fuzz`.

2. **libFuzzer (`cargo-fuzz`)** — этот каталог. Отдельный крейт вне workspace,
   потому что нужен nightly и санитайзеры.

   | цель | что проверяет |
   |---|---|
   | `scan` | сканер не паникует, вердикт согласован с находками, результат детерминирован |
   | `sanitize` | стерильная копия всегда проверена повторно и идемпотентна |
   | `wire` | декодеры конверта и отчёта (то, что хост читает у недоверенного гостя) |
   | `zip`, `cfb`, `xml` | контейнеры и безопасный XML-ридер по отдельности |

   ```sh
   cargo install cargo-fuzz --version 0.12.0 --locked
   mkdir -p fuzz/corpus/scan
   ARCA_CORPUS_OUT=$PWD/fuzz/corpus/scan cargo test -p arca-xray dump_corpus   # затравка
   ARCA_WIRE_OUT=$PWD/fuzz/corpus/wire   cargo test -p arca-xray dump_wire_seeds
   cd fuzz && cargo +nightly fuzz run scan -- -dict=dictionaries/ooxml.dict -max_total_time=300
   ```

   Корпуса, артефакты и `target/` в репозиторий не попадают (`.gitignore`):
   в них могут оказаться вредоносные образцы.

> Статус: цели собираются и проходят на всём корпусе через заглушку `fuzz_target!`
> (проверено в среде разработки, где нет `libfuzzer-sys` и nightly), но
> **сами по себе в `libFuzzer` ещё ни разу не запускались**. Первый прогон —
> задача CI (`.github/workflows/fuzz.yml`) или машины с доступом к crates.io.
