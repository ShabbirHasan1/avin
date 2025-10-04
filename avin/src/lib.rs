/*****************************************************************************
 * CREATED:     2023.07.23 15:06
 * URL:         http://avin.info
 * AUTHOR:      Alex Avin
 * E-MAIL:      mr.alexavin@gmail.com
 * LICENSE:     MIT
 ****************************************************************************/

//! # AVIN  -  Ars Vincere (лат. искусство побеждать)
//! ```text
//!                             Open source cross-platform trading system
//!                                      __   _    _  ___  __   _
//!                                     /__\   \  /    |   | \  |
//!                                    |    |   \/    _|_  |  \_|
//!
//! ```
//!
//! # ru
//! ## Начало работы (Getting start)
//!
//! Чтобы как то подступиться к работе с системой, рассмотрим элементарные
//! примеры, как вообще работать с системой, и как писать свои стратегии.
//!
//! Для работы нужен тинькофф токен, аккаунт на Московской бирже,
//! настроеный конфиг, биржевые данные. С этого и начнем.
//!
//! ### Тинькофф токен.
//! Что такое токен, и как его выпустить, смотрите [официальную инструкцию
//! Т-Банка](https://developer.tbank.ru/invest/intro/intro/token)
//!
//! ### MOEX аккаунт.
//! Для загрузки данных с Московской биржы, там нужно зарегистрироваться.
//! Регистрация бесплатная: <https://passport.moex.com/registration>
//!
//! С этой регистрацией доступны свечи и тики за сегодня. Остальные
//! рыночные данные по платной подписке:
//! <https://data.moex.com/products/algopack>
//!
//! ### Config
//! Образец файла смотрите в репозатарии
//! <https://github.com/arsvincere/avin/blob/master/res/config.toml>
//!
//! Все настройки пользователся задаются в нем. Отредактируйте его под себя
//! (как минимум задайте пути к папке где вы будете работать, и папке с
//! рыночными данными). В остальном можно использовать и дефолтный.
//!
//! Переместите файл в ~/.config/avin/config.toml
//!
//! ### Загрузка рыночных данных.
//! На данный момент доступная загрузка рыночных данных только с Московской
//! биржи. Сделана утилита с элементарным cli интерфейсом.
//!
//! Установка утилиты avin-data
//! ```bash
//! git clone --depth=1 https://github.com/arsvincere/avin.git
//! cd avin
//! make install
//! ```
//!
//! Программа устанавливается в ~/.local/bin. Проверьте что этот путь
//! добавлен в PATH. Если нет:
//! ```bash
//! export PATH=$HOME/.local/bin:$PATH
//! ```
//!
//! Первое что нужно сделать - кэшировать информацию о доступных инструментах.
//! ```bash
//! avin-data cache
//! ```
//!
//! Поиск инструмента:
//! ```bash
//! avin-data find -i "moex_share_sber"
//! ```
//!
//! Загрузка всех имеющихся рыночных данных по инструменту:
//! ```bash
//! avin-data download -i "moex_share_sber"
//! ```
//!
//! Посмотреть другие возможности программы:
//! ```bash
//! avin-data --help
//! ```
//!
//! Посмотреть доступные опции для команды, например "download":
//! ```bash
//! avin-data download --help
//! ```

pub use avin_analyse as analyse;
pub use avin_connect as connect;
pub use avin_core as core;
pub use avin_data as data;
pub use avin_gui as gui;
pub use avin_scanner as scanner;
pub use avin_simulator as simulator;
pub use avin_strategy as strategy;
pub use avin_tester as tester;
pub use avin_trader as trader;
pub use avin_utils as utils;
