<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# Arca для Android — исходники

Справочник (поддерживаемые версии и экраны, безопасность, сборка, что проверено, ограничения) —
[`docs/ANDROID.md`](../docs/ANDROID.md). Порядок выпуска — [`docs/RELEASING.md`](../docs/RELEASING.md).

```text
android/
├── app/src/main/
│   ├── AndroidManifest.xml          # без разрешений; фильтры «Открыть с помощью» (content://, 31 расширение)
│   ├── res/                         # values (en), values-ru, values-night, drawable, mipmap-anydpi-v26, xml
│   └── java/org/arca/viewer/
│       ├── App.java, MainActivity.java, Session.java, Engine.java, Native.java, Prefs.java
│       ├── core/                    # чистая Java без Android: JSON, модель, форматирование, пороги, кэш страниц
│       └── ui/                      # экраны (программный UI), Compat — всё, что зависит от версии Android
├── core-test/                       # тесты чистой Java-логики (JVM): bash android/core-test/run.sh
└── host-test/                       # JNI на настоящей JVM (-Xcheck:jni): bash android/host-test/run.sh
```

* Зависимостей нет: ни AndroidX, ни Gradle, ни сторонних библиотек Java. Пакет собирает
  `tools/android/build-apk.sh` (aapt2 → javac → d8 → zipalign → apksigner).
* Всё, что новее API 26, — только в `ui/Compat.java` за проверками `SDK_INT`;
  `tools/android/check-android.sh` компилирует приложение против API 26 и 36 и это проверяет.
* Добавляя строку, добавляйте её в `values/` и `values-ru/`: `arca-devtools check-resources`
  (`crates/arca-devtools`) сверяет оба языка и идентификаторы движка.
* Интерфейс строится в коде. Новый экран — подкласс `ui/Screen`; любое состояние документа — в
  `Session` (переживает пересоздание `Activity`).
