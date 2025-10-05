/*****************************************************************************
 * URL:         http://avin.info
 * AUTHOR:      Alex Avin
 * E-MAIL:      mr.alexavin@gmail.com
 * LICENSE:     MIT
 ****************************************************************************/

use std::collections::HashMap;

use bitcode::{Decode, Encode};
use chrono::{DateTime, Utc};

use avin_utils::CFG;

use crate::Transaction;

/// Exchange operation, create when order fulfilled.
///
/// # ru
/// Операция, создается после полного исполнения ордера.
///
/// Содержит временную метку timestamp nanos, количество, сумму и комиссию.
/// Количество указывается не в лотах, а в бумагах.
#[derive(Debug, PartialEq, Encode, Decode, Clone)]
pub struct Operation {
    pub ts: i64,
    pub quantity: i32,
    pub value: f64,
    pub commission: f64,
}
impl Operation {
    /// Create new operation.
    ///
    /// # ru
    /// Конструктор.
    pub fn new(ts: i64, quantity: i32, value: f64, commission: f64) -> Self {
        Self {
            ts,
            quantity,
            value,
            commission,
        }
    }
    /// Build operation from timestamp, transactions and commission.
    ///
    /// # ru
    /// Создает операцию из временной метки, списка транзакций и суммы
    /// комиссии.
    ///
    /// Ордера на бирже исполняются отдельными транзакциями. Брокер
    /// присылает их по факту совершения. В конце, когда ордер полностью
    /// исполнен, брокер суммирует транзакции, крепит к ним комиссию, и
    /// присылает готовую операцию.
    ///
    /// В качестве времени операции используется время последней транзакции.
    pub fn build(
        ts: i64,
        transactions: &[Transaction],
        commission: f64,
    ) -> Self {
        if transactions.is_empty() {
            panic!("Empty transactions list! Fail to create operation!");
        }

        let mut quantity: i32 = 0;
        let mut value: f64 = 0.0;
        for i in transactions.iter() {
            quantity += i.quantity;
            value += i.quantity as f64 * i.price;
        }

        Self {
            ts,
            quantity,
            value,
            commission,
        }
    }
    /// Create operation from bin format
    ///
    /// # ru
    /// Создает операцию из бинарного формата, который использует
    /// тестер и трейдер для сохранения на диске.
    pub fn from_bin(bytes: &[u8]) -> Self {
        bitcode::decode(bytes).unwrap()
    }
    /// Create vector bytes from operation, for saving.
    ///
    /// # ru
    /// Преобразует операцию в бинарный формат для сохранения на диске.
    pub fn to_bin(&self) -> Vec<u8> {
        bitcode::encode(self)
    }
    /// dead code, may be deleted soon
    #[deprecated]
    pub fn from_csv(csv: &str) -> Self {
        let parts: Vec<&str> = csv.split(';').collect();

        let ts: i64 = parts[0].parse().unwrap();
        let quantity: i32 = parts[1].parse().unwrap();
        let value: f64 = parts[2].parse().unwrap();
        let commission: f64 = parts[3].parse().unwrap();

        Operation {
            ts,
            quantity,
            value,
            commission,
        }
    }
    /// dead code, may be deleted soon
    #[deprecated]
    pub fn to_csv(&self) -> String {
        format!(
            "{};{};{};{};",
            self.ts, self.quantity, self.value, self.commission
        )
    }
    /// dead code, may be deleted soon
    #[deprecated]
    pub fn to_hash_map(&self) -> HashMap<&str, String> {
        let mut info = HashMap::new();
        info.insert("ts", self.ts.to_string());
        info.insert("quantity", self.quantity.to_string());
        info.insert("value", self.value.to_string());
        info.insert("commission", self.commission.to_string());

        info
    }

    /// Return DateTime UTC of operation
    ///
    /// # ru
    /// Возвращает дату и время операции в UTC таймзоне
    #[inline]
    pub fn dt(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_nanos(self.ts)
    }
    /// Return average price of operation
    ///
    /// # ru
    /// Возвращает среднюю цену по операции. Может быть нужно,
    /// если ордер был рыночный и транзакции исполнены по разным ценам.
    #[inline]
    pub fn avg_price(&self) -> f64 {
        self.value / self.quantity as f64
    }
}
impl std::fmt::Display for Operation {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let dt = format!("{}", self.dt().format(&CFG.usr.dt_fmt));
        write!(
            f,
            "Operation={} {}={}+{}",
            dt, self.quantity, self.value, self.commission
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn new() {
        let ts = 100500;
        let t1 = Transaction::new(10, 320.0);
        let t2 = Transaction::new(10, 330.0);

        let op = Operation::build(ts, &[t1, t2], 6500.0 * 0.001);
        assert_eq!(op.ts, ts);
        assert_eq!(op.quantity, 20);
        assert_eq!(op.value, 6500.0);
        assert_eq!(op.commission, 6.5);
        assert_eq!(op.avg_price(), 325.0);
    }
    #[test]
    #[allow(deprecated)]
    fn csv() {
        let t1 = Transaction::new(10, 320.0);

        let dt = Utc.with_ymd_and_hms(2025, 4, 6, 12, 19, 0).unwrap();
        let ts = dt.timestamp_nanos_opt().unwrap();
        let op = Operation::build(ts, &[t1], 320.0 * 10.0 * 0.0005);

        let csv = op.to_csv();
        assert_eq!(csv, "1743941940000000000;10;3200;1.6;");

        let from_csv = Operation::from_csv(&csv);
        assert_eq!(op, from_csv);
    }
    #[test]
    fn bin() {
        let t1 = Transaction::new(10, 320.0);

        let dt = Utc.with_ymd_and_hms(2025, 4, 6, 12, 19, 0).unwrap();
        let ts = dt.timestamp_nanos_opt().unwrap();
        let op = Operation::build(ts, &[t1], 320.0 * 10.0 * 0.0005);

        let bytes = op.to_bin();
        let decoded = Operation::from_bin(&bytes);
        assert_eq!(op, decoded);
    }
}
