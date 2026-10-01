<div align="center">

[English](README.md) · **Русский**

# ✈️ FlyXTogether

**Летайте на одном самолёте вместе с другом в X-Plane 12: бесплатно и с открытым кодом.**

Плагин общей кабины (multicrew) в духе SmartCopilot и SharedFlight.
Два пилота, два симулятора, один самолёт, напрямую через интернет.
Без аккаунтов, серверов и подписок.

[![build](https://github.com/hymaxo/FlyXTogether/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/hymaxo/FlyXTogether/actions/workflows/build.yml)
[![release](https://img.shields.io/github/v/release/hymaxo/FlyXTogether?include_prereleases&sort=semver&label=release)](https://github.com/hymaxo/FlyXTogether/releases)
[![X-Plane 12](https://img.shields.io/badge/X--Plane-12.4%2B-1f6feb)](https://www.x-plane.com/)
[![license](https://img.shields.io/badge/license-MIT%20%2F%20Apache--2.0-green)](#-лицензия)

</div>

```
   🧑‍✈️ Пилотирующий (PF)                        👩‍✈️ Контролирующий (PM)
  ┌──────────────────┐     ваш интернет      ┌──────────────────┐
  │  X-Plane 12      │ ◀───────────────────▶ │  X-Plane 12      │
  │  ведёт самолёт   │  шифрованно, напрямую │  та же кабина,   │
  │                  │                       │  «управляю я»    │
  └──────────────────┘                       └──────────────────┘
```

> [!WARNING]
> **Это альфа-версия.** В наших тестах всё работает, но это ранняя стадия.
> Возможны шероховатости, поэтому пока не стоит полагаться на неё в
> 12-часовом онлайн-ивенте. Нашли баг? [Создайте issue](https://github.com/hymaxo/FlyXTogether/issues),
> это очень помогает! 💛

## 🛩️ Что уже работает

- **Любой самолёт, без настройки.** FlyXTogether сам читает файлы кабины
  самолёта и понимает, что синхронизировать. **Cessna 172 SP** (обычная,
  **G1000** и **гидроплан**) проверена; остальные самолёты в окне помечены
  как *непроверенные*, но часто работают отлично (мы ради интереса
  слетали на Citation X).
- **Общая кабина.** Тумблеры, ручки, радио, настройки автопилота, топливо и
  магнето синхронизируются в обе стороны. Кнопки и удерживаемые команды
  (например, стартер) срабатывают на обоих местах.
- **Передача управления.** Любой пилот может сказать «управление беру»:
  кнопкой **Take controls**, через меню Plugins или назначенной клавишей.
  Самолёт продолжает полёт без рывков.
- **Живые приборы у PM.** X-Plane контролирующего пилота продолжает
  симуляцию, поэтому все приборы, двигатели, электрика и авионика работают
  сами, а штурвал, РУД и триммер пилотирующего двигаются у вас на глазах.
- **Плавно на реальном интернете.** Буфер воспроизведения скрывает джиттер
  и потерю пакетов, полёт выглядит плавно, без телепортаций.
- **Напрямую и приватно.** Сессии с паролем, сквозное шифрование
  (QUIC + TLS). Ничего не идёт через чужие серверы.
- **Бережно к X-Plane.** Ошибки внутри плагина перехватываются, а самолёт
  всегда возвращается вам, когда сессия заканчивается.

## 🚧 Пока нет (будет позже)

- Подключение в полёте: соединение работает, но у самолётов, где положение
  тумблеров хранится в их собственных скриптах (например, Citation X),
  кабины могут разойтись. Пока подключайтесь на земле.
- Общий план полёта (FMS/GPS) на обоих местах
- Импорт профилей SmartCopilot и проверенные профили для других самолётов
- Подключение без проброса портов (relay или обход NAT)
- Проверка на macOS, Linux и в VR. Сборки есть, но на них ещё никто не
  летал. Будем рады отчётам!

## 📦 Установка

1. Скачайте `FlyXTogether-<версия>.zip` со страницы
   [**Releases**](https://github.com/hymaxo/FlyXTogether/releases).
2. Распакуйте в `X-Plane 12/Resources/plugins/`. Должно получиться
   `Resources/plugins/FlyXTogether/win_x64/FlyXTogether.xpl`
   (плюс `mac_x64/` и `lin_x64/`).
3. Запустите X-Plane. Плагин в меню **Plugins → FlyXTogether**.

У обоих пилотов должны быть **одинаковая версия FlyXTogether** и **один и тот
же самолёт** (0.2 не подключается к 0.1).

<details>
<summary>🍎 macOS: разрешить неподписанный плагин</summary>

Плагин пока не нотаризован, поэтому macOS блокирует его после скачивания.
Один раз после распаковки выполните:

```bash
xattr -dr com.apple.quarantine "/path/to/X-Plane 12/Resources/plugins/FlyXTogether"
```

</details>

<details>
<summary>🌙 Ночные сборки</summary>

Каждый коммит в `main` собирается автоматически и публикуется как
пре-релиз [**nightly**](https://github.com/hymaxo/FlyXTogether/releases/tag/nightly).
В нём самое новое, включая самые новые баги. Оба пилота должны использовать
одну и ту же сборку.

</details>

## 🎮 Полетели вместе за 60 секунд

**Капитан (хост):**

1. Загрузите Cessna 172 SP (или любой самолёт, на земле).
2. **Plugins → FlyXTogether → Open**, вкладка **Host**, задайте пароль, нажмите **Host**.
3. Пробросьте UDP-порт **49700** на роутере на свой компьютер.
4. Отправьте второму пилоту свой внешний IP-адрес и пароль.

**Второй пилот (подключение):**

1. Загрузите тот же самолёт и вариант.
2. **Plugins → FlyXTogether → Open**, вкладка **Join**, вставьте адрес, введите пароль, нажмите **Join**.
3. Сначала пилотирует хост. Работайте с радио, читайте чек-лист и нажмите
   **Take controls**, когда ваша очередь вести самолёт. **Leave session**
   возвращает вам ваш собственный самолёт.

Ваш джойстик и РУД управляют самолётом, только пока вы PF. Хотите изменить,
что синхронизируется для самолёта? Смотрите
**[профили самолётов](docs/profiles.md)** (на английском).

Проброс портов, CGNAT, Tailscale/ZeroTier и значение каждого сообщения в окне
описаны в **[руководстве по хостингу](docs/hosting.md)** (на английском).

## 🐞 Сообщить о проблеме

Плагин ведёт журнал рядом с собой:
`Resources/plugins/FlyXTogether/FlyXTogether.log`. После вылета журнал
предыдущего запуска лежит в `FlyXTogether.previous.log`. Пожалуйста,
приложите его (и `Log.txt` из X-Plane) к issue. Пароли никогда не пишутся в
журнал.

## 🛠️ Сборка из исходников

Понадобятся [rustup](https://rustup.rs/) и компилятор C++ (Dear ImGui
собирается из исходников). Версия Rust закреплена в `rust-toolchain.toml` и
устанавливается автоматически.

- **Windows:** Visual Studio Build Tools с компонентом C++
- **macOS:** `xcode-select --install`, затем один раз `scripts/fetch-xplane-sdk.sh`
- **Linux:** `sudo apt install build-essential libgl-dev`

```bash
git clone https://github.com/hymaxo/FlyXTogether.git
cd FlyXTogether
cargo test --workspace
cargo build -p flyx-plugin --release
```

Собрать и сразу скопировать в ваш X-Plane (Git Bash в Windows, любая
оболочка в остальных системах). `--dev` добавляет пункты меню для
разработчиков:

```bash
XPLANE_ROOT="/path/to/X-Plane 12" scripts/dev-install.sh
```

<details>
<summary>Ещё заметки для разработчиков</summary>

Библиотека плагина собирается в `target/release/`:

| Платформа | Собранный файл | Установить как |
|---|---|---|
| Windows | `FlyXTogether.dll` | `FlyXTogether/win_x64/FlyXTogether.xpl` |
| macOS | `libFlyXTogether.dylib` | `FlyXTogether/mac_x64/FlyXTogether.xpl` |
| Linux | `libFlyXTogether.so` | `FlyXTogether/lin_x64/FlyXTogether.xpl` |

`tools/flyx-peer` — консольный пир, который может хостить или подключаться без
X-Plane. Удобен, чтобы тестировать плагин в одиночку, например
`cargo run -p flyx-peer -- host --flight circuit`.

`crates/flyx-xplm/src/sys.rs` генерируется из заголовков X-Plane SDK скриптом
`scripts/gen-bindings.sh` (нужны `bindgen-cli` и libclang; в Windows добавьте
папку libclang в `PATH` и в `LIBCLANG_PATH`).

| Путь | Содержимое |
|---|---|
| `crates/flyx-protocol` | типы сетевых сообщений |
| `crates/flyx-sync` | состояния сессии, передача управления, регистр кабины, определения синхронизации и воспроизведение полёта (без X-Plane) |
| `crates/flyx-net` | транспорт QUIC и проверка пароля |
| `crates/flyx-xplm` | безопасные Rust-обёртки над X-Plane SDK |
| `crates/flyx-plugin` | сам плагин |
| `tools/flyx-peer` | консольный тестовый пир |

**Релизы:** поднимите `version` в `Cargo.toml`, добавьте раздел в
`CHANGELOG.md` и запушьте тег `v<версия>`. GitHub Actions соберёт все три
платформы и опубликует релиз.

</details>

## 📜 Лицензия

На ваш выбор: [MIT](LICENSE-MIT) или [Apache-2.0](LICENSE-APACHE).
X-Plane SDK в `third_party/xplane-sdk/` распространяется под собственной лицензией.

<div align="center">

Сделано с ☕ и слишком большим количеством тач-энд-гоу. Чистого неба! 🌤️

</div>
