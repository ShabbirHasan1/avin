/*****************************************************************************
 * URL:         http://avin.info
 * AUTHOR:      Alex Avin
 * E-MAIL:      mr.alexavin@gmail.com
 * LICENSE:     MIT
 ****************************************************************************/

use std::collections::HashMap;

use chrono::{DateTime, Datelike, Timelike, Utc};
use tonic::transport::{Channel, ClientTlsConfig};

use avin_core::{
    Account, Bar, BarEvent, Category, Direction, Event, FilledMarketOrder,
    Iid, LimitOrder, MarketOrder, NewLimitOrder, NewMarketOrder,
    NewStopOrder, Operation, Order, PostedLimitOrder, PostedMarketOrder,
    PostedStopOrder, RejectedLimitOrder, RejectedMarketOrder, Share,
    StopOrder, StopOrderKind, Tic, TicEvent, TimeFrame, Transaction,
};
use avin_utils::{self as utils, CFG, Cmd};

use super::api;
use super::interceptor::DefaultInterceptor;
use api::instruments::instruments_service_client::InstrumentsServiceClient;
use api::marketdata::market_data_request::Payload as Req;
use api::marketdata::market_data_response::Payload as Res;
use api::marketdata::{
    CandleInstrument, InfoInstrument, MarketDataRequest, MarketDataResponse,
    SubscribeCandlesRequest, SubscribeInfoRequest, SubscribeTradesRequest,
    SubscriptionAction, SubscriptionInterval, TradeInstrument,
    market_data_service_client::MarketDataServiceClient,
    market_data_stream_service_client::MarketDataStreamServiceClient,
};
use api::operations::operations_service_client::OperationsServiceClient;
use api::orders::TradesStreamRequest;
use api::orders::orders_service_client::OrdersServiceClient;
use api::orders::orders_stream_service_client::OrdersStreamServiceClient;
use api::stoporders::stop_orders_service_client::StopOrdersServiceClient;
use api::users::users_service_client::UsersServiceClient;

type T = tonic::service::interceptor::InterceptedService<
    Channel,
    DefaultInterceptor,
>;
pub struct TinkoffClient {
    channel: Option<Channel>,
    interceptor: Option<DefaultInterceptor>,

    users: Option<UsersServiceClient<T>>,
    instruments: Option<InstrumentsServiceClient<T>>,
    orders: Option<OrdersServiceClient<T>>,
    stoporders: Option<StopOrdersServiceClient<T>>,
    operations: Option<OperationsServiceClient<T>>,
    marketdata: Option<MarketDataServiceClient<T>>,
    marketdata_stream: Option<MarketDataStreamServiceClient<T>>,
    data_stream_tx: Option<flume::Sender<MarketDataRequest>>,

    event_tx: tokio::sync::mpsc::UnboundedSender<Event>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
impl TinkoffClient {
    pub fn new(event_tx: tokio::sync::mpsc::UnboundedSender<Event>) -> Self {
        // create self
        Self {
            channel: None,
            interceptor: None,
            users: None,
            instruments: None,
            orders: None,
            stoporders: None,
            operations: None,
            marketdata: None,
            marketdata_stream: None,
            data_stream_tx: None,

            event_tx,
            tasks: Vec::new(),
        }
    }

    // start loop
    pub async fn connect(&mut self) -> Result<(), &'static str> {
        self.interceptor = Some(TinkoffClient::create_interceptor());
        self.channel = Some(TinkoffClient::create_channel().await);

        // create clients
        self.users = Some(UsersServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.instruments = Some(InstrumentsServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.orders = Some(OrdersServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.stoporders = Some(StopOrdersServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.operations = Some(OperationsServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.marketdata = Some(MarketDataServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        ));
        self.marketdata_stream =
            Some(MarketDataStreamServiceClient::with_interceptor(
                self.channel.clone().unwrap(),
                self.interceptor.clone().unwrap(),
            ));

        self.create_marketdata_stream().await.unwrap();
        self.create_transactions_stream().await.unwrap();

        Ok(())
    }
    pub async fn create_marketdata_stream(
        &mut self,
    ) -> Result<(), &'static str> {
        // NOTE: Подписка на инфу по Сбер банку
        // по сберу можно будет потом отслеживать открыт ли рынок
        // Плюс это костыль, чтобы сразу при создании брокера запустить
        // дата стрим, и потом при каждой подписке на бары или тики не
        // проверять его наличие. А чтобы его создать надо на что-то
        // подписаться вот и подписываюсь на инфу по сберу. Функция
        // вызывается при запуске брокера.

        // create request
        let sber_figi = "BBG004730N88";
        let info_instrument = InfoInstrument {
            figi: "".to_string(),
            instrument_id: sber_figi.to_string(),
        };
        let request = MarketDataRequest {
            payload: Some(Req::SubscribeInfoRequest(SubscribeInfoRequest {
                subscription_action: SubscriptionAction::Subscribe as i32,
                instruments: vec![info_instrument],
            })),
        };

        // create channel
        let (tx, rx) = flume::unbounded();

        // send request
        tx.send(request).unwrap();
        let response = self
            .marketdata_stream
            .as_mut()
            .unwrap()
            .market_data_stream(rx.into_stream())
            .await
            .unwrap();

        // get stream
        let stream = response.into_inner();

        // get sender
        let sender = self.event_tx.clone();

        // run loop
        let task = tokio::spawn(async move {
            start_marketdata_stream(stream, sender).await
        });

        // save stream tx and task handle
        self.data_stream_tx = Some(tx);
        self.tasks.push(task);

        Ok(())
    }
    pub async fn create_transactions_stream(
        &mut self,
    ) -> Result<(), &'static str> {
        let acc = self.get_account("Agni").await.unwrap();

        // create request
        let request = TradesStreamRequest {
            accounts: vec![acc.id().clone()],
        };

        // create client
        let client = OrdersStreamServiceClient::with_interceptor(
            self.channel.clone().unwrap(),
            self.interceptor.clone().unwrap(),
        );

        // get sender
        let sender = self.event_tx.clone();

        // run loop
        let task = tokio::spawn(async move {
            start_transaction_stream(request, client, sender).await
        });

        // save stream tx and task handle
        self.tasks.push(task);

        Ok(())
    }

    // instrument info
    pub async fn get_shares(&mut self) -> Result<Vec<Share>, &'static str> {
        // create request
        // api::instrument::InstrumentStatus = 1 - это инструменты
        // доступные для торговли через TINKOFF INVEST API, то есть
        // все кроме внебиржевых бумаг.
        let request =
            tonic::Request::new(api::instruments::InstrumentsRequest {
                instrument_status: 1,
            });

        // send request
        let response = self
            .instruments
            .as_mut()
            .unwrap()
            .shares(request)
            .await
            .unwrap();
        // api::instruments::SharesResponse
        let message = response.into_parts();
        // api::instruments::Share
        let mut t_instruments = message.1.instruments;

        // convert tinkoff::api::instruments::Share -> avin::Share
        let mut a_shares = Vec::new();
        while let Some(t_share) = t_instruments.pop() {
            let a_share: Share = t_share.into();

            // NOTE: пока торгую только на мосбирже поэтому сделаю
            // фильтр биржы только MOEX, без американских и китайских акций
            if a_share.exchange() == "MOEX" {
                a_shares.push(a_share);
            }
        }

        Ok(a_shares)
    }

    // account
    pub async fn get_accounts(
        &mut self,
    ) -> Result<Vec<Account>, &'static str> {
        // create request
        let request = tonic::Request::new(api::users::GetAccountsRequest {});

        // send request
        let response = self
            .users
            .as_mut()
            .unwrap()
            .get_accounts(request)
            .await
            .unwrap();
        // api::users::GetAccountsResponse
        let message = response.into_parts();
        // vec[api::users::Account]
        let t_accounts = message.1.accounts;

        // convert tinkoff::api::users::Account -> avin::Account
        let mut accounts = Vec::new();
        for i in t_accounts.iter() {
            let a = Account::new(&i.name, &i.id);
            accounts.push(a);
        }

        Ok(accounts)
    }
    pub async fn get_account(
        &mut self,
        name: &str,
    ) -> Result<Account, &'static str> {
        // create request
        let request = tonic::Request::new(api::users::GetAccountsRequest {});

        // send request
        let response = self
            .users
            .as_mut()
            .unwrap()
            .get_accounts(request)
            .await
            .unwrap();
        let message = response.into_parts();
        let t_accounts = message.1.accounts; // api::users::Account

        // convert tinkoff::api::users::Account -> avin::Account
        for i in t_accounts.iter() {
            if i.name == name {
                let a = Account::new(&i.name, &i.id);
                return Ok(a);
            }
        }

        Err("account not found")
    }
    pub async fn get_limit_orders(
        &mut self,
        a: &Account,
        iid: &Iid,
    ) -> Result<Vec<LimitOrder>, &'static str> {
        // create request
        let request = tonic::Request::new(api::orders::GetOrdersRequest {
            account_id: a.id().to_string(),
        });

        // send request
        let response = self
            .orders
            .as_mut()
            .unwrap()
            .get_orders(request)
            .await
            .unwrap();
        // api::orders::GetOrdersResponse
        let message = response.into_parts();
        // vec[api::orders::OrderState]
        let mut t_orders = message.1.orders;

        // convert tinkoff::api::orders::OrderState -> avin::LimitOrder
        let mut a_orders = Vec::new();
        while let Some(t_order) = t_orders.pop() {
            if &t_order.figi == iid.figi() {
                let a_order: LimitOrder = t_order.into();
                a_orders.push(a_order);
            }
        }

        Ok(a_orders)
    }
    pub async fn get_stop_orders(
        &mut self,
        a: &Account,
        iid: &Iid,
    ) -> Result<Vec<StopOrder>, &'static str> {
        // create request
        let request =
            tonic::Request::new(api::stoporders::GetStopOrdersRequest {
                account_id: a.id().to_string(),
            });

        // send request
        let response = self
            .stoporders
            .as_mut()
            .unwrap()
            .get_stop_orders(request)
            .await
            .unwrap();
        // api::stoporders::GetStopOrdersResponse
        let message = response.into_parts();
        // vec[api::stoporders::StopOrder]
        let mut t_orders = message.1.stop_orders;

        // convert tinkoff::api::orders::OrderState -> avin::LimitOrder
        let mut a_orders = Vec::new();
        while let Some(t_order) = t_orders.pop() {
            if &t_order.figi == iid.figi() {
                let a_order: StopOrder = t_order.into();
                a_orders.push(a_order);
            }
        }

        Ok(a_orders)
    }
    pub async fn get_operation(
        &mut self,
        a: &Account,
        order: &Order,
    ) -> Result<Operation, &'static str> {
        // create request
        let request =
            tonic::Request::new(api::orders::GetOrderStateRequest {
                account_id: a.id().to_string(),
                order_id: order.broker_id().unwrap().clone(),
            });

        // send request
        let response = self
            .orders
            .as_mut()
            .unwrap()
            .get_order_state(request)
            .await
            .unwrap();
        // api::orders::GetOrderStateResponse
        let message = response.into_parts();
        // api::orders::OrderState
        let t_order = message.1;

        // convert tinkoff::api::orders::OrderState -> avin::Operation
        let operation: Operation = t_order.into();

        Ok(operation)
    }
    pub async fn get_operations(
        &mut self,
        a: &Account,
        iid: &Iid,
        from: Option<&DateTime<Utc>>,
        till: Option<&DateTime<Utc>>,
    ) -> Result<Vec<Operation>, &'static str> {
        // create request
        let from = match from {
            Some(from) => {
                let ts = prost_types::Timestamp::date_time(
                    from.year() as i64,
                    from.month() as u8,
                    from.day() as u8,
                    from.hour() as u8,
                    from.minute() as u8,
                    from.second() as u8,
                )
                .unwrap();
                Some(ts)
            }
            None => None,
        };
        let to = match till {
            Some(till) => {
                let ts = prost_types::Timestamp::date_time(
                    till.year() as i64,
                    till.month() as u8,
                    till.day() as u8,
                    till.hour() as u8,
                    till.minute() as u8,
                    till.second() as u8,
                )
                .unwrap();
                Some(ts)
            }
            None => None,
        };
        let request =
            tonic::Request::new(api::operations::OperationsRequest {
                account_id: a.id().to_string(),
                from,
                to,
                state: api::operations::OperationState::Executed as i32,
                figi: iid.figi().clone(),
            });

        // send request
        let response = self
            .operations
            .as_mut()
            .unwrap()
            .get_operations(request)
            .await
            .unwrap();
        // api::operations::OperationsResponse
        let message = response.into_parts();
        // vec[api::operations::Operation]
        let mut t_operations = message.1.operations;

        // convert tinkoff::api::operations::Operation -> avin::Operation
        let mut a_operations = Vec::new();
        while let Some(t_operation) = t_operations.pop() {
            if &t_operation.figi == iid.figi() {
                let a_operation: Operation = t_operation.into();
                a_operations.push(a_operation);
            }
        }

        Ok(a_operations)
    }

    // orders
    pub async fn post_market(
        &mut self,
        a: &Account,
        iid: &Iid,
        order: NewMarketOrder,
    ) -> Result<Order, &'static str> {
        // create request
        let direction: api::orders::OrderDirection =
            order.direction.clone().into();
        // let request = tonic::Request::new(api::orders::PostOrderRequest {
        //     figi: String::new(),
        //     quantity: order.lots as i64,
        //     price: None,
        //     direction: direction as i32,
        //     account_id: a.id().to_string(),
        //     order_type: 2, // api::orders::OrderType::Market
        //     order_id: uuid::Uuid::new_v4().to_string(),
        //     instrument_id: iid.figi().clone(),
        // });
        let request = api::orders::PostOrderRequest {
            figi: String::new(),
            quantity: order.lots as i64,
            price: None,
            direction: direction as i32,
            account_id: a.id().to_string(),
            order_type: 2, // api::orders::OrderType::Market
            order_id: uuid::Uuid::new_v4().to_string(),
            instrument_id: iid.figi().clone(),
        };

        // send request
        let response =
            match self.orders.as_mut().unwrap().post_order(request).await {
                Ok(response) => response,
                Err(why) => {
                    log::error!("{why:?}");
                    return Err("post order failed");
                }
            };
        let message = response.into_parts();
        // api::orders::PostOrderResponse
        let t_post_order_response = message.1;

        // NOTE: PostOrderResponse содержит недостаточно информации,
        // в нем нет транзакций например, а маркет ордер после выставления
        // сразу же принимает статус ExecutionReportStatusFill. А чтобы
        // собрать crate::FilledMarketOrder мне нужны и транзакции и
        // операцию собрать, поэтому сразу запрашиваем OrderState

        // create request
        let request =
            tonic::Request::new(api::orders::GetOrderStateRequest {
                account_id: a.id().to_string(),
                order_id: t_post_order_response.order_id,
            });

        // send request
        let response = self
            .orders
            .as_mut()
            .unwrap()
            .get_order_state(request)
            .await
            .unwrap();
        let message = response.into_parts();
        // api::orders::OrderState
        let t_order = message.1;

        // convert tinkoff::api::orders::OrderState -> avin::MarketOrder
        let order: MarketOrder = t_order.into();
        let order = Order::Market(order);

        Ok(order)
    }
    pub async fn post_limit(
        &mut self,
        a: &Account,
        iid: &Iid,
        order: NewLimitOrder,
    ) -> Result<Order, &'static str> {
        // create request
        let direction: api::orders::OrderDirection =
            order.direction.clone().into();
        let request = tonic::Request::new(api::orders::PostOrderRequest {
            figi: String::new(),
            quantity: order.lots as i64,
            price: Some(order.price.into()),
            direction: direction as i32,
            account_id: a.id().to_string(),
            order_type: 1, // api::orders::OrderType::Limit
            order_id: uuid::Uuid::new_v4().to_string(),
            instrument_id: iid.figi().clone(),
        });

        // send request
        let response =
            match self.orders.as_mut().unwrap().post_order(request).await {
                Ok(response) => response,
                Err(_) => {
                    return Err("post order failed");
                }
            };
        let message = response.into_parts();
        // api::orders::PostOrderResponse
        let t_order = message.1;

        // convert api::orders::PostOrderResponse -> avin::LimitOrder
        let a_order: LimitOrder = t_order.into();
        let a_order = Order::Limit(a_order);

        Ok(a_order)
    }
    pub async fn post_stop(
        &mut self,
        a: &Account,
        iid: &Iid,
        order: NewStopOrder,
    ) -> Result<StopOrder, &'static str> {
        // create request
        let last_price = self.get_last_price(iid).await.unwrap();
        let t_order_type = t_stop_order_type(&order, last_price);
        let t_exec_price = match order.exec_price {
            Some(price) => {
                let q: api::stoporders::Quotation = price.into();
                Some(q)
            }
            None => None,
        };
        let t_stop_price = {
            let q: api::stoporders::Quotation = order.stop_price.into();
            Some(q)
        };
        let direction: api::stoporders::StopOrderDirection =
            order.direction.clone().into();
        let request =
            tonic::Request::new(api::stoporders::PostStopOrderRequest {
                figi: String::new(),
                quantity: order.lots as i64,
                price: t_exec_price,
                stop_price: t_stop_price,
                direction: direction as i32,
                account_id: a.id().to_string(),
                expiration_type: 1, // StopOrderExpirationType::GoodTillCancel
                stop_order_type: t_order_type,
                expire_date: None,
                instrument_id: iid.figi().clone(),
            });

        // send request
        let response = match self
            .stoporders
            .as_mut()
            .unwrap()
            .post_stop_order(request)
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return Err("post stop order failed");
            }
        };
        let message = response.into_parts();
        // api::orders::PostStopOrderResponse
        let t_post_stop_order_response = message.1;

        // change order status
        let order = order.post(&t_post_stop_order_response.stop_order_id);
        // wrap
        let order = StopOrder::Posted(order);

        Ok(order)
    }
    pub async fn cancel_limit(
        &mut self,
        a: &Account,
        order: PostedLimitOrder,
    ) -> Result<LimitOrder, &'static str> {
        // create request
        let request = tonic::Request::new(api::orders::CancelOrderRequest {
            account_id: a.id().to_string(),
            order_id: order.broker_id.clone(),
        });

        // send request
        let tonic_resp =
            match self.orders.as_mut().unwrap().cancel_order(request).await {
                Ok(response) => response,
                Err(_) => {
                    return Err("cancel order failed");
                }
            };
        // api::orders::CancelOrderResponse
        let response = tonic_resp.into_parts().1;

        // check time of cancel order, it shoud be != 0
        if response.time.unwrap().seconds == 0 {
            return Err("cancel order failed");
        }

        // change order status
        let canceled_order = order.cancel();
        // wrap
        let order = LimitOrder::Canceled(canceled_order);

        Ok(order)
    }
    pub async fn cancel_stop(
        &mut self,
        a: &Account,
        order: PostedStopOrder,
    ) -> Result<StopOrder, &'static str> {
        // create request
        let request =
            tonic::Request::new(api::stoporders::CancelStopOrderRequest {
                account_id: a.id().to_string(),
                stop_order_id: order.broker_id.clone(),
            });

        // send request
        let tonic_resp = match self
            .stoporders
            .as_mut()
            .unwrap()
            .cancel_stop_order(request)
            .await
        {
            Ok(response) => response,
            Err(_) => {
                return Err("cancel stop order failed");
            }
        };
        // api::orders::CancelOrderResponse
        let response = tonic_resp.into_parts().1;

        // check time of cancel order, it shoud be != 0
        if response.time.unwrap().seconds == 0 {
            return Err("cancel order failed");
        }

        // change order status
        let canceled_order = order.cancel();
        // wrap
        let order = StopOrder::Canceled(canceled_order);

        Ok(order)
    }

    // market data
    pub async fn get_bars(
        &mut self,
        iid: &Iid,
        tf: TimeFrame,
        from: DateTime<Utc>,
        till: DateTime<Utc>,
    ) -> Result<Vec<Bar>, &'static str> {
        // create request
        let from = {
            let ts = prost_types::Timestamp::date_time(
                from.year() as i64,
                from.month() as u8,
                from.day() as u8,
                from.hour() as u8,
                from.minute() as u8,
                from.second() as u8,
            )
            .unwrap();
            Some(ts)
        };
        let to = {
            let ts = prost_types::Timestamp::date_time(
                till.year() as i64,
                till.month() as u8,
                till.day() as u8,
                till.hour() as u8,
                till.minute() as u8,
                till.second() as u8,
            )
            .unwrap();
            Some(ts)
        };
        let interval: api::marketdata::CandleInterval = (tf).into();
        let request =
            tonic::Request::new(api::marketdata::GetCandlesRequest {
                figi: "".to_string(),
                from,
                to,
                interval: interval as i32,
                instrument_id: iid.figi().clone(),
            });

        // send request
        let response = self
            .marketdata
            .as_mut()
            .unwrap()
            .get_candles(request)
            .await
            .unwrap();

        // api::marketdata::GetCandlesResponse
        let message = response.into_parts();
        // vec[api::marketdata::HistoricCandle]
        let t_candles = message.1.candles;

        // convert api::marketdata::HistoricCandle -> crate::Bar
        let mut bars = Vec::new();
        for candle in t_candles {
            if candle.is_complete {
                let bar: Bar = candle.into();
                bars.push(bar);
            }
        }

        Ok(bars)
    }
    pub async fn get_last_price(
        &mut self,
        iid: &Iid,
    ) -> Result<f64, &'static str> {
        // create request
        let request =
            tonic::Request::new(api::marketdata::GetLastPricesRequest {
                figi: vec!["".to_string()],
                instrument_id: vec![iid.figi().clone()],
            });

        // send request
        let response = self
            .marketdata
            .as_mut()
            .unwrap()
            .get_last_prices(request)
            .await
            .unwrap();
        // api::marketdata::GetLastPricesResponse
        let message = response.into_parts();
        // vec[api::marketdata::LastPrice]
        let mut t_prices = message.1.last_prices;

        if t_prices.len() == 1 {
            let t_price = t_prices.pop().unwrap(); // LastPrice
            let t_price = t_price.price.unwrap(); // Quotation
            let price: f64 = t_price.into();
            return Ok(price);
        }

        Err("Fail to get last price")
    }
    pub async fn subscribe_info(
        &mut self,
        iid: &Iid,
    ) -> Result<(), &'static str> {
        // create request
        let info_instrument = InfoInstrument {
            figi: "".to_string(),
            instrument_id: iid.figi().clone(),
        };
        let request = MarketDataRequest {
            payload: Some(Req::SubscribeInfoRequest(SubscribeInfoRequest {
                subscription_action: SubscriptionAction::Subscribe as i32,
                instruments: vec![info_instrument],
            })),
        };

        // send request in existed stream
        self.data_stream_tx.as_mut().unwrap().send(request).unwrap();

        Ok(())
    }
    pub async fn subscribe_bar(
        &mut self,
        iid: &Iid,
        tf: &TimeFrame,
    ) -> Result<(), &'static str> {
        // create request
        let interval: SubscriptionInterval = (*tf).into();
        let candle_instrument = CandleInstrument {
            figi: "".to_string(),
            // interval: SubscriptionInterval::OneMinute as i32,
            interval: interval as i32,
            instrument_id: iid.figi().clone(),
        };
        let request = MarketDataRequest {
            payload: Some(Req::SubscribeCandlesRequest(
                SubscribeCandlesRequest {
                    subscription_action: SubscriptionAction::Subscribe as i32,
                    instruments: vec![candle_instrument],
                    waiting_close: false,
                },
            )),
        };

        // send request in existed stream
        self.data_stream_tx.as_mut().unwrap().send(request).unwrap();

        Ok(())
    }
    pub async fn subscribe_tic(
        &mut self,
        iid: &Iid,
    ) -> Result<(), &'static str> {
        // create request
        let instrument = TradeInstrument {
            figi: "".to_string(),
            instrument_id: iid.figi().clone(),
        };
        let request = MarketDataRequest {
            payload: Some(Req::SubscribeTradesRequest(
                SubscribeTradesRequest {
                    subscription_action: SubscriptionAction::Subscribe as i32,
                    instruments: vec![instrument],
                },
            )),
        };

        // send request in existed stream
        self.data_stream_tx.as_mut().unwrap().send(request).unwrap();

        Ok(())
    }
    pub async fn unsubscribe_bar(
        &mut self,
        iid: &Iid,
    ) -> Result<(), &'static str> {
        // create request
        let candle_instrument = CandleInstrument {
            figi: "".to_string(),
            interval: SubscriptionInterval::OneMinute as i32,
            instrument_id: iid.figi().clone(),
        };
        let request = MarketDataRequest {
            payload: Some(Req::SubscribeCandlesRequest(
                SubscribeCandlesRequest {
                    subscription_action: SubscriptionAction::Unsubscribe
                        as i32,
                    instruments: vec![candle_instrument],
                    waiting_close: false,
                },
            )),
        };

        self.data_stream_tx.as_mut().unwrap().send(request).unwrap();

        Ok(())
    }
    pub async fn unsubscribe_tic(
        &mut self,
        _iid: &Iid,
    ) -> Result<(), &'static str> {
        todo!();
    }

    // private
    fn create_interceptor() -> DefaultInterceptor {
        // load token
        let path = CFG.connect.tinkoff();
        let token = Cmd::read(&path).unwrap().trim().to_string();

        // create interceptor
        DefaultInterceptor { token }
    }
    async fn create_channel() -> Channel {
        let tls = ClientTlsConfig::new();
        let target = "https://invest-public-api.tinkoff.ru:443/";

        Channel::from_static(target)
            .tls_config(tls)
            .unwrap()
            .connect()
            .await
            .unwrap()
    }
}

// stream loops
async fn start_marketdata_stream(
    mut data_stream: tonic::codec::Streaming<MarketDataResponse>,
    sender: tokio::sync::mpsc::UnboundedSender<Event>,
) {
    // receive market data
    while let Some(msg) = data_stream.message().await.unwrap() {
        match msg.payload.unwrap() {
            // market data
            Res::Candle(candle) => {
                // log::debug!("{candle:?}");
                let e: BarEvent = candle.into();
                sender.send(Event::Bar(e)).unwrap();
            }
            Res::Trade(tic) => {
                // log::debug!("{tic:?}");
                let e: TicEvent = tic.into();
                sender.send(Event::Tic(e)).unwrap();
            }
            Res::Orderbook(_) => todo!(),
            Res::TradingStatus(_) => {
                // log::debug!("{i:#?}");
                log::warn!("Сделать обработку смены статуса актива!")
            }
            Res::LastPrice(_) => todo!(),

            // subscription responses
            Res::SubscribeInfoResponse(_) => {
                // log::debug!(":: Subscribe info {r:?}");
            }
            Res::SubscribeTradesResponse(_) => {
                // log::debug!(":: Subscribe trades {r:?}");
            }
            Res::SubscribeCandlesResponse(_) => {
                // log::debug!(":: Subscribe candles {r:?}");
            }
            Res::SubscribeOrderBookResponse(_) => {
                // log::debug!(":: Subscribe book {r:?}");
            }
            Res::SubscribeLastPriceResponse(_) => {
                // log::debug!(":: Subscribe last price {r:?}");
            }
            Res::Ping(_) => {}
        }
    }
    log::error!("STREAM STOPED");
    panic!("И че делать?");
}
async fn start_transaction_stream(
    request: api::orders::TradesStreamRequest,
    mut client: api::orders::orders_stream_service_client::OrdersStreamServiceClient<tonic::service::interceptor::InterceptedService<Channel, DefaultInterceptor>>,
    _sender: tokio::sync::mpsc::UnboundedSender<Event>,
) {
    // send request
    let response = client.trades_stream(request).await.unwrap();

    // get stream
    let mut transaction_stream = response.into_inner();

    while let Some(msg) = transaction_stream.message().await.unwrap() {
        log::debug!("---- TS: {msg:?}");
        // TODO:
        // короче здесь я получаю транзакции, а надо собрать
        // OrderEvent и его отправить
    }
}

// from Tinkoff to avin
impl From<api::orders::MoneyValue> for f64 {
    fn from(t: api::orders::MoneyValue) -> f64 {
        let frac: f64 = t.nano as f64 / 1_000_000_000.0;

        t.units as f64 + frac
    }
}
impl From<api::stoporders::MoneyValue> for f64 {
    fn from(t: api::stoporders::MoneyValue) -> f64 {
        let frac: f64 = t.nano as f64 / 1_000_000_000.0;

        t.units as f64 + frac
    }
}
impl From<api::instruments::Quotation> for f64 {
    fn from(t: api::instruments::Quotation) -> f64 {
        let frac: f64 = t.nano as f64 / 1_000_000_000.0;

        t.units as f64 + frac
    }
}
impl From<api::marketdata::Quotation> for f64 {
    fn from(t: api::marketdata::Quotation) -> f64 {
        let frac: f64 = t.nano as f64 / 1_000_000_000.0;

        t.units as f64 + frac
    }
}
impl From<api::marketdata::HistoricCandle> for Bar {
    fn from(value: api::marketdata::HistoricCandle) -> Self {
        let ts_nanos = {
            let ts = value.time.unwrap();
            DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
                .unwrap()
                .timestamp_nanos_opt()
                .unwrap()
        };

        Bar {
            ts_nanos,
            o: value.open.unwrap().into(),
            h: value.high.unwrap().into(),
            l: value.low.unwrap().into(),
            c: value.close.unwrap().into(),
            v: value.volume as u64,
        }
    }
}
impl From<api::instruments::Share> for Share {
    fn from(t: api::instruments::Share) -> Self {
        let step: f64 = match t.min_price_increment {
            Some(s) => s.into(),
            None => 0.0, // бывает для инструментов которые уже не торгуются
        };
        let dlong: f64 = match t.dlong {
            Some(s) => s.into(),
            None => 1.0,
        };
        let dshort: f64 = match t.dshort {
            Some(s) => s.into(),
            None => 1.0,
        };
        let dlong_min: f64 = match t.dlong_min {
            Some(s) => s.into(),
            None => 1.0,
        };
        let dshort_min: f64 = match t.dshort_min {
            Some(s) => s.into(),
            None => 1.0,
        };

        let first_1m_dt = match t.first_1min_candle_date {
            Some(ts) => DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
                .unwrap()
                .to_rfc3339(),
            None => String::new(),
        };
        let first_d_dt = match t.first_1day_candle_date {
            Some(ts) => DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
                .unwrap()
                .to_rfc3339(),
            None => String::new(),
        };

        let mut info = HashMap::new();
        info.insert("exchange".to_string(), std_exchange_name(&t.exchange));
        info.insert("exchange_specific".to_string(), t.exchange);
        info.insert("category".to_string(), Category::SHARE.to_string());
        info.insert("ticker".to_string(), t.ticker);
        info.insert("figi".to_string(), t.figi);
        info.insert("country".to_string(), t.country_of_risk);
        info.insert("currency".to_string(), t.currency);
        info.insert("sector".to_string(), t.sector);
        info.insert("class_code".to_string(), t.class_code);
        info.insert("isin".to_string(), t.isin);
        info.insert("uid".to_string(), t.uid);
        info.insert("name".to_string(), t.name);
        info.insert("lot".to_string(), t.lot.to_string());
        info.insert("step".to_string(), step.to_string());
        info.insert("long".to_string(), dlong.to_string());
        info.insert("short".to_string(), dshort.to_string());
        info.insert("long_qual".to_string(), dlong_min.to_string());
        info.insert("short_qual".to_string(), dshort_min.to_string());
        info.insert("first_1m".to_string(), first_1m_dt);
        info.insert("first_d".to_string(), first_d_dt);

        Share::from_info(info)
    }
}
impl From<api::orders::OrderDirection> for Direction {
    fn from(t: api::orders::OrderDirection) -> Self {
        match t {
            api::orders::OrderDirection::Buy => Direction::Buy,
            api::orders::OrderDirection::Sell => Direction::Sell,
            api::orders::OrderDirection::Unspecified => panic!(),
        }
        // if t == 1 {
        //     Direction::Buy
        // } else if t == 2 {
        //     Direction::Sell
        // } else {
        //     panic!();
        // }
    }
}
impl From<api::orders::OrderState> for MarketOrder {
    fn from(t: api::orders::OrderState) -> Self {
        // Example:
        //     OrderState {
        //         order_id: "64168707676",
        //         execution_report_status: ExecutionReportStatusNew,
        //         lots_requested: 1,
        //         lots_executed: 0,
        //         initial_order_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 3000,
        //                 nano: 0,
        //             },
        //         ),
        //         executed_order_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         total_order_amount: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 3000,
        //                 nano: 0,
        //             },
        //         ),
        //         average_position_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         initial_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 1,
        //                 nano: 200000000,
        //             },
        //         ),
        //         executed_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         figi: "BBG004730N88",
        //         direction: Buy,
        //         initial_security_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 300,
        //                 nano: 0,
        //             },
        //         ),
        //         stages: [],
        //         service_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         currency: "rub",
        //         order_type: Limit,
        //         order_date: Some(
        //             Timestamp {
        //                 seconds: 1744549166,
        //                 nanos: 798701000,
        //             },
        //         ),
        //         instrument_uid: "e6123145-9665-43e0-8413-cd61b8aa9b13",
        //         order_request_id: "976d581b-d21c-4367-b1b2-cbffdf69b35d",
        //     },
        // ]

        let status = t.execution_report_status();
        let operation: Operation = t.clone().into();
        let direction: Direction = t.direction().into();
        let lots = t.lots_requested as u32;
        let broker_id = t.order_id;
        let mut transactions = Vec::new();
        for order_stage in t.stages {
            let t = order_stage.into(); // api::orders::OrderStage
            transactions.push(t);
        }

        use api::orders::OrderExecutionReportStatus as s;

        match status {
            s::ExecutionReportStatusFill => {
                let order = FilledMarketOrder {
                    direction,
                    lots,
                    broker_id,
                    transactions,
                    operation,
                };
                MarketOrder::Filled(order)
            }
            s::ExecutionReportStatusNew => {
                let order = PostedMarketOrder {
                    direction,
                    lots,
                    broker_id,
                    transactions,
                };
                MarketOrder::Posted(order)
            }
            s::ExecutionReportStatusRejected => {
                let order = RejectedMarketOrder {
                    direction,
                    lots,
                    meta: "".to_string(),
                };
                MarketOrder::Rejected(order)
            }
            s::ExecutionReportStatusCancelled => {
                todo!()
            }
            s::ExecutionReportStatusUnspecified => {
                todo!()
            }
            s::ExecutionReportStatusPartiallyfill => {
                let order = PostedMarketOrder {
                    direction,
                    lots,
                    broker_id,
                    transactions,
                };
                MarketOrder::Posted(order)
            }
        }
    }
}
impl From<api::orders::OrderState> for LimitOrder {
    fn from(t: api::orders::OrderState) -> Self {
        // Example:
        //     OrderState {
        //         order_id: "64168707676",
        //         execution_report_status: ExecutionReportStatusNew,
        //         lots_requested: 1,
        //         lots_executed: 0,
        //         initial_order_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 3000,
        //                 nano: 0,
        //             },
        //         ),
        //         executed_order_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         total_order_amount: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 3000,
        //                 nano: 0,
        //             },
        //         ),
        //         average_position_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         initial_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 1,
        //                 nano: 200000000,
        //             },
        //         ),
        //         executed_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         figi: "BBG004730N88",
        //         direction: Buy,
        //         initial_security_price: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 300,
        //                 nano: 0,
        //             },
        //         ),
        //         stages: [],
        //         service_commission: Some(
        //             MoneyValue {
        //                 currency: "rub",
        //                 units: 0,
        //                 nano: 0,
        //             },
        //         ),
        //         currency: "rub",
        //         order_type: Limit,
        //         order_date: Some(
        //             Timestamp {
        //                 seconds: 1744549166,
        //                 nanos: 798701000,
        //             },
        //         ),
        //         instrument_uid: "e6123145-9665-43e0-8413-cd61b8aa9b13",
        //         order_request_id: "976d581b-d21c-4367-b1b2-cbffdf69b35d",
        //     },
        // ]

        let direction: Direction = t.direction().into();

        let mut transactions = Vec::new();
        for order_stage in t.stages {
            let t = order_stage.into(); // api::orders::OrderStage
            transactions.push(t);
        }

        let posted_limit_order = PostedLimitOrder {
            direction,
            lots: t.lots_requested as u32,
            price: t.initial_security_price.unwrap().into(),
            broker_id: t.order_id,
            transactions,
        };

        LimitOrder::Posted(posted_limit_order)
    }
}
impl From<api::orders::OrderState> for Operation {
    fn from(t: api::orders::OrderState) -> Self {
        // Example:
        // OrderState {
        //     order_id: "64542282209",
        //     execution_report_status: ExecutionReportStatusFill,
        //     lots_requested: 1,
        //     lots_executed: 1,
        //     initial_order_price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 84,
        //             nano: 830000000,
        //         },
        //     ),
        //     executed_order_price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 84,
        //             nano: 560000000,
        //         },
        //     ),
        //     total_order_amount: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 84,
        //             nano: 560000000,
        //         },
        //     ),
        //     average_position_price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 84,
        //             nano: 560000000,
        //         },
        //     ),
        //     initial_commission: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 0,
        //             nano: 50000000,
        //         },
        //     ),
        //     executed_commission: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 0,
        //             nano: 40000000,
        //         },
        //     ),
        //     figi: "BBG004730ZJ9",
        //     direction: Buy,
        //     initial_security_price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 84,
        //             nano: 830000000,
        //         },
        //     ),
        //     stages: [
        //         OrderStage {
        //             price: Some(
        //                 MoneyValue {
        //                     currency: "rub",
        //                     units: 84,
        //                     nano: 560000000,
        //                 },
        //             ),
        //             quantity: 1,
        //             trade_id: "12967455671",
        //         },
        //     ],
        //     service_commission: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 0,
        //             nano: 0,
        //         },
        //     ),
        //     currency: "rub",
        //     order_type: Market,
        //     order_date: Some(
        //         Timestamp {
        //             seconds: 1745222367,
        //             nanos: 657172000,
        //         },
        //     ),
        //     instrument_uid: "8e2b0325-0292-4654-8a18-4f63ed3b0e09",
        //     order_request_id: "9bca5b4b-a9c1-4d3e-8415-31ab39e28534",
        // }

        // timestamp
        let ts = t.order_date.unwrap();
        let ts = DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap();

        // transactions
        let mut transactions = Vec::new();
        for order_stage in t.stages {
            let t = order_stage.into(); // api::orders::OrderStage
            transactions.push(t);
        }

        // commission
        let commission = t.executed_commission.unwrap().into();

        Operation::build(ts, &transactions, commission)
    }
}
impl From<api::orders::OrderStage> for Transaction {
    fn from(t: api::orders::OrderStage) -> Self {
        Transaction::new(t.quantity as i32, t.price.unwrap().into())
    }
}
impl From<api::orders::PostOrderResponse> for LimitOrder {
    fn from(t: api::orders::PostOrderResponse) -> Self {
        // TODO: а может ну его нафиг этот метод? и сделать
        // дополнительный запрос ордер стейт, и из него уже
        // формировать ордер, как сделано в post_market
        // А то здесь например если лимитку выставить так что она
        // сразу исполнится то не получится собрать FilledLimitOrder
        // из PostOrderResponse, не достаточно в нем данных, нет
        // транзакций
        use api::orders::OrderExecutionReportStatus as status;

        match t.execution_report_status() {
            status::ExecutionReportStatusUnspecified => {
                todo!();
            }

            status::ExecutionReportStatusFill => {
                todo!();
            }

            status::ExecutionReportStatusRejected => {
                let order = RejectedLimitOrder {
                    direction: t.direction().into(),
                    lots: t.lots_requested as u32,
                    price: t.initial_security_price.unwrap().into(),
                    meta: String::new(), // TODO: logger.error(t)
                };
                LimitOrder::Rejected(order)
            }

            status::ExecutionReportStatusCancelled => {
                todo!();
            }

            status::ExecutionReportStatusNew => {
                let order = PostedLimitOrder {
                    direction: t.direction().into(),
                    lots: t.lots_requested as u32,
                    price: t.initial_security_price.unwrap().into(),
                    broker_id: t.order_id,
                    transactions: Vec::new(),
                };
                LimitOrder::Posted(order)
            }

            status::ExecutionReportStatusPartiallyfill => {
                todo!();
            }
        }
    }
}
impl From<api::stoporders::StopOrderDirection> for Direction {
    fn from(t: api::stoporders::StopOrderDirection) -> Self {
        use api::stoporders::StopOrderDirection as d;
        match t {
            d::Buy => Direction::Buy,
            d::Sell => Direction::Sell,
            d::Unspecified => panic!(),
        }
    }
}
impl From<api::stoporders::StopOrderType> for StopOrderKind {
    fn from(value: api::stoporders::StopOrderType) -> Self {
        // pub enum StopOrderType {
        //     Unspecified = 0,
        //     TakeProfit = 1,
        //     StopLoss = 2,
        //     StopLimit = 3,
        // }
        use api::stoporders::StopOrderType as sot;
        match value {
            sot::TakeProfit => StopOrderKind::TakeProfit,
            sot::StopLoss => StopOrderKind::StopLoss,
            sot::StopLimit => StopOrderKind::StopLoss,
            sot::Unspecified => panic!(),
        }
    }
}
impl From<api::stoporders::StopOrder> for StopOrder {
    fn from(t: api::stoporders::StopOrder) -> Self {
        // Example:
        // StopOrder {
        //     stop_order_id: "6310200d-9903-4740-b001-1d1906c38946",
        //     lots_requested: 1,
        //     figi: "BBG004730N88",
        //     direction: Buy,
        //     currency: "rub",
        //     order_type: TakeProfit,
        //     create_date: Some(
        //         Timestamp {
        //             seconds: 1745157606,
        //             nanos: 113476000,
        //         },
        //     ),
        //     activation_date_time: None,
        //     expiration_time: None,
        //     price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 299,
        //             nano: 610000000,
        //         },
        //     ),
        //     stop_price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 299,
        //             nano: 610000000,
        //         },
        //     ),
        //     instrument_uid: "e6123145-9665-43e0-8413-cd61b8aa9b13",
        // }

        let direction: Direction = t.direction().into();
        let kind: StopOrderKind = t.order_type().into();

        let exec_price = match t.price {
            // NOTE: Тинькофф на стоп ордера с рыночным исполнением присылает
            // значение не None, а вот такое:
            // Some(MoneyValue { currence: "", units: 0, nanos: 0})
            // Так что его заменяет на None
            Some(money_value) => {
                if money_value.currency.is_empty()
                    && money_value.units == 0
                    && money_value.nano == 0
                {
                    None
                } else
                // а если там что то другое то преобразовываем в f64
                {
                    let price: f64 = money_value.into();
                    Some(price)
                }
            }
            // в реальности эта ветка никогда не выполняется, тинькофф
            // никогда не присылает None для поля "price", см. NOTE выше
            None => None,
        };

        let posted_stop_order = PostedStopOrder {
            kind,
            direction,
            lots: t.lots_requested as u32,
            stop_price: t.stop_price.unwrap().into(),
            exec_price,
            broker_id: t.stop_order_id,
        };

        StopOrder::Posted(posted_stop_order)
    }
}
impl From<api::operations::Operation> for Operation {
    fn from(_t: api::operations::Operation) -> Self {
        // Example:
        // Operation {
        //     id: "65576085",
        //     parent_operation_id: "",
        //     currency: "rub",
        //     payment: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: -2513,
        //             nano: -190000000,
        //         },
        //     ),
        //     price: Some(
        //         MoneyValue {
        //             currency: "rub",
        //             units: 125,
        //             nano: 659500000,
        //         },
        //     ),
        //     state: Executed,
        //     quantity: 20,
        //     quantity_rest: 0,
        //     figi: "BBG004730N88",
        //     instrument_type: "share",
        //     date: Some(
        //         Timestamp {
        //             seconds: 1660206646,
        //             nanos: 695000000,
        //         },
        //     ),
        //     r#type: "Покупка ценных бумаг",
        //     operation_type: Buy,
        //     trades: [
        //         OperationTrade {
        //             trade_id: "32718582",
        //             date_time: Some(
        //                 Timestamp {
        //                     seconds: 1660206646,
        //                     nanos: 695000000,
        //                 },
        //             ),
        //             quantity: 20,
        //             price: Some(
        //                 MoneyValue {
        //                     currency: "rub",
        //                     units: 125,
        //                     nano: 659500000,
        //                 },
        //             ),
        //         },
        //     ],
        //     asset_uid: "40d89385-a03a-4659-bf4e-d3ecba011782",
        //     position_uid: "41eb2102-5333-4713-bf15-72b204c4bf7b",
        //     instrument_uid: "e6123145-9665-43e0-8413-cd61b8aa9b13",
        // }

        // NOTE: вообще не очень понятно зачем мне этот метод.
        // Тинькофф вернет список операций в котором будут операции
        // покупки продажи и коммиссия по ним будут отдельными операциями.
        // И мне в свой формат их собирать неудобно. Мне проще из ордер
        // стрейта получить операцию.
        // И еще тут приходят всякие другие операции: пополнение счета,
        // налоги, начисление вариационной маржи... когда нибудь это
        // надо будет реализовать, но сейчас не нужно.
        todo!("TODO_ME");
    }
}
impl From<api::marketdata::SubscriptionInterval> for TimeFrame {
    fn from(value: api::marketdata::SubscriptionInterval) -> Self {
        // Оригинальные SubscriptionInterval сгенерированный из proto
        // pub enum SubscriptionInterval {
        //     Unspecified = 0,
        //     OneMinute = 1,
        //     FiveMinutes = 2,
        // }

        // HACK: однако в python SDK вроде работает подписка на другие
        // интервалы... взял значения от туда, вроде работают
        // class SubscriptionInterval(_grpc_helpers.Enum):
        //     SUBSCRIPTION_INTERVAL_UNSPECIFIED = 0
        //     SUBSCRIPTION_INTERVAL_ONE_MINUTE = 1
        //     SUBSCRIPTION_INTERVAL_FIVE_MINUTES = 2
        //     SUBSCRIPTION_INTERVAL_FIFTEEN_MINUTES = 3
        //     SUBSCRIPTION_INTERVAL_ONE_HOUR = 4
        //     SUBSCRIPTION_INTERVAL_ONE_DAY = 5
        //     SUBSCRIPTION_INTERVAL_2_MIN = 6
        //     SUBSCRIPTION_INTERVAL_3_MIN = 7
        //     SUBSCRIPTION_INTERVAL_10_MIN = 8
        //     SUBSCRIPTION_INTERVAL_30_MIN = 9
        //     SUBSCRIPTION_INTERVAL_2_HOUR = 10
        //     SUBSCRIPTION_INTERVAL_4_HOUR = 11
        //     SUBSCRIPTION_INTERVAL_WEEK = 12
        //     SUBSCRIPTION_INTERVAL_MONTH = 13
        use api::marketdata::SubscriptionInterval as si;
        match value {
            si::OneMinute => TimeFrame::M1,
            si::FiveMinutes => todo!(),
            si::TenMinutes => TimeFrame::M10,
            si::OneHour => TimeFrame::H1,
            si::Day => TimeFrame::Day,
            si::Week => TimeFrame::Week,
            si::Month => TimeFrame::Month,
            si::Unspecified => panic!("WTF???"),
        }
    }
}
impl From<api::marketdata::TradeDirection> for Direction {
    fn from(value: api::marketdata::TradeDirection) -> Self {
        use api::marketdata::TradeDirection as td;

        match value {
            td::Buy => Direction::Buy,
            td::Sell => Direction::Sell,
            td::Unspecified => panic!("WTF???"),
        }
    }
}
impl From<api::marketdata::Candle> for BarEvent {
    fn from(value: api::marketdata::Candle) -> Self {
        let tf: TimeFrame = value.interval().into();
        let figi = value.figi;

        let ts = value.time.unwrap();
        let ts_nanos = DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap();
        let bar = Bar {
            ts_nanos,
            o: value.open.unwrap().into(),
            h: value.high.unwrap().into(),
            l: value.low.unwrap().into(),
            c: value.close.unwrap().into(),
            v: value.volume as u64,
        };

        BarEvent { bar, tf, figi }
    }
}
impl From<api::marketdata::Trade> for TicEvent {
    fn from(t: api::marketdata::Trade) -> Self {
        let direction: Direction = t.direction().into();

        let figi = t.figi;
        let iid = avin_core::Manager::find_figi(&figi).unwrap();
        let lot = iid.lot();

        let ts = t.time.unwrap();
        let ts_nanos = DateTime::from_timestamp(ts.seconds, ts.nanos as u32)
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap();
        let lots = t.quantity as u32;
        let price: f64 = t.price.unwrap().into();
        let value = lots as f64 * price * lot as f64;

        let tic = Tic {
            ts_nanos,
            direction,
            lots,
            price,
            value,
        };

        TicEvent { figi, tic }
    }
}
fn std_exchange_name(exchange_name: &str) -> String {
    let exchange_name = exchange_name.to_uppercase();

    // values as "MOEX_PLUS", "MOEX_WEEKEND"... => "MOEX"
    if exchange_name.contains("MOEX") {
        return String::from("MOEX");
    }

    // values as "SPB_RU_MORNING"... => "SPB"
    if exchange_name.contains("SPB") {
        return String::from("SPB");
    }

    // FUTURE - у них биржа указана FORTS_EVENING, но похеру
    // пока для простоты ставлю им тоже биржу MOEX
    if exchange_name.contains("FORTS") {
        return String::from("MOEX");
    }

    // CURRENCY - у них биржа указана FX, но похеру
    // пока для простоты ставлю им тоже биржу MOEX
    if exchange_name.contains("FX") {
        return String::from("MOEX");
    }

    // там всякая странная хуйня еще есть в биржах
    // "otc_ncc", "LSE_MORNING", "Issuance", "unknown"...
    // Часть из них по факту американские биржи, по которым сейчас
    // один хрен торги не доступны, внебирживые еще, и хз еще какие, я всем
    // этим не торгую, поэтому сейчас ставим всем непонятным активам
    // биржу пустую строку
    String::new()
}

// from avin to Tinkoff
impl From<f64> for api::orders::Quotation {
    fn from(value: f64) -> Self {
        let units = value.floor() as i64;
        let nano = (utils::round(value.fract(), 9) * 1_000_000_000.0) as i32;

        api::orders::Quotation { units, nano }
    }
}
impl From<f64> for api::stoporders::Quotation {
    fn from(value: f64) -> Self {
        let units = value.floor() as i64;
        let nano = (utils::round(value.fract(), 9) * 1_000_000_000.0) as i32;

        api::stoporders::Quotation { units, nano }
    }
}
impl From<Direction> for api::orders::OrderDirection {
    fn from(value: Direction) -> Self {
        use api::orders::OrderDirection as od;

        match value {
            Direction::Buy => od::Buy,
            Direction::Sell => od::Sell,
        }
    }
}
impl From<Direction> for api::stoporders::StopOrderDirection {
    fn from(value: Direction) -> Self {
        use api::stoporders::StopOrderDirection as sod;

        match value {
            Direction::Buy => sod::Buy,
            Direction::Sell => sod::Sell,
        }
    }
}
impl From<TimeFrame> for api::marketdata::CandleInterval {
    fn from(value: TimeFrame) -> Self {
        use api::marketdata::CandleInterval as ci;

        match value {
            TimeFrame::M1 => ci::CandleInterval1Min,
            // TimeFrame::M5 => ci::CandleInterval5Min,
            TimeFrame::M10 => ci::CandleInterval10Min,
            TimeFrame::H1 => ci::Hour,
            TimeFrame::Day => ci::Day,
            TimeFrame::Week => ci::Week,
            TimeFrame::Month => ci::Month,
        }
    }
}
impl From<TimeFrame> for api::marketdata::SubscriptionInterval {
    fn from(value: TimeFrame) -> Self {
        use api::marketdata::SubscriptionInterval as si;

        match value {
            TimeFrame::M1 => si::OneMinute,
            // TimeFrame::M5 => si::___,
            TimeFrame::M10 => si::TenMinutes,
            TimeFrame::H1 => si::OneHour,
            TimeFrame::Day => si::Day,
            TimeFrame::Week => si::Week,
            TimeFrame::Month => si::Month,
        }
    }
}
fn t_stop_order_type(order: &NewStopOrder, last_price: f64) -> i32 {
    // Tinkoff типы:
    // pub enum StopOrderType {
    //     Unspecified = 0,
    //     TakeProfit = 1,
    //     StopLoss = 2,
    //     StopLimit = 3,
    // }

    // невозможно выставить стоп ордер по цене которая уже есть
    // так что он сразу триггернется
    if order.stop_price == last_price {
        panic!();
    }

    if order.direction == Direction::Buy {
        if last_price > order.stop_price {
            return 1; // take profit
        }
        if last_price < order.stop_price {
            if order.exec_price.is_none() {
                return 2; // stop loss
            }
            if order.exec_price.is_some() {
                return 3; // stop limit
            }
        }
    }

    if order.direction == Direction::Sell {
        if last_price < order.stop_price {
            return 1; // take profit
        }
        if last_price > order.stop_price {
            if order.exec_price.is_none() {
                return 2; // stop loss
            }
            if order.exec_price.is_some() {
                return 3; // stop limit
            }
        }
    }

    // здесь мы никогда не должны оказаться
    panic!("WTF???");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tinkoff::api;
    use chrono::{TimeDelta, Utc};

    #[test]
    fn quotation() {
        let price: f64 = 84.05;
        let q: api::orders::Quotation = price.into();
        assert_eq!(q.units, 84);
        assert_eq!(q.nano, 50000000);

        let price: f64 = 100.15;
        let q: api::orders::Quotation = price.into();
        assert_eq!(q.units, 100);
        assert_eq!(q.nano, 150000000);
    }

    #[tokio::test]
    #[ignore]
    async fn get_shares() {
        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        let shares = b.get_shares().await.unwrap();
        assert!(shares.len() > 100);
    }

    #[tokio::test]
    #[ignore]
    async fn get_account() {
        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        let a = b.get_account("Agni").await.unwrap();
        assert_eq!(a.name(), "Agni");
        assert_eq!(a.id(), "2193020159");
    }

    #[tokio::test]
    #[ignore]
    async fn get_accounts() {
        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        let acc = b.get_accounts().await.unwrap();
        assert!(acc.len() >= 4);
    }

    #[tokio::test]
    #[ignore]
    async fn get_last_price() {
        // share, iid
        let share = Share::new("moex_share_vtbr").unwrap();
        let iid = share.iid();

        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        let price = b.get_last_price(iid).await.unwrap();
        assert!(price > 0.0);
        assert!(price < 100500.0);
    }

    #[tokio::test]
    #[ignore]
    async fn get_bars() {
        // share, iid
        let share = Share::new("moex_share_vtbr").unwrap();
        let iid = share.iid();

        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        // timeframe
        let tf = TimeFrame::Day;

        // from till
        let seconds_in_year = 365 * 24 * 60 * 60;
        let till = Utc::now();
        let from = till - TimeDelta::new(seconds_in_year, 0).unwrap();

        // get bars
        let bars = b.get_bars(iid, tf, from, till).await.unwrap();
        assert!(bars.len() > 200);
    }

    #[tokio::test]
    #[ignore]
    async fn post_market_order() {
        // share, iid
        let share = Share::new("moex_share_vtbr").unwrap();
        let iid = share.iid();

        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        // get account
        let a = b.get_account("Agni").await.unwrap();

        let new_order = MarketOrder::new(Direction::Buy, 1);
        let order = b.post_market(&a, iid, new_order.clone()).await.unwrap();
        if let Order::Market(MarketOrder::Filled(filled_order)) = order {
            assert_eq!(filled_order.direction, new_order.direction);
            assert_eq!(filled_order.lots, new_order.lots);
        } else {
            unreachable!();
        }

        let new_order = MarketOrder::new(Direction::Sell, 1);
        let order = b.post_market(&a, iid, new_order.clone()).await.unwrap();
        if let Order::Market(MarketOrder::Filled(filled_order)) = order {
            assert_eq!(filled_order.direction, new_order.direction);
            assert_eq!(filled_order.lots, new_order.lots);
        } else {
            unreachable!();
        }
    }

    #[tokio::test]
    #[ignore]
    async fn post_limit_order() {
        // share, iid
        let share = Share::new("moex_share_vtbr").unwrap();
        let iid = share.iid();

        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        // get account
        let a = b.get_account("Agni").await.unwrap();

        // get last price
        let price = b.get_last_price(iid).await.unwrap();

        // create order
        let new_order = LimitOrder::new(Direction::Buy, 1, price - 3.0);

        // post order
        let order = b.post_limit(&a, iid, new_order.clone()).await.unwrap();

        // get limit orders
        let limit_orders = b.get_limit_orders(&a, iid).await.unwrap();
        assert!(!limit_orders.is_empty());

        // cancel order if "Posted"
        if let Order::Limit(LimitOrder::Posted(posted_order)) = order {
            assert_eq!(posted_order.direction, new_order.direction);
            assert_eq!(posted_order.lots, new_order.lots);
            b.cancel_limit(&a, posted_order).await.unwrap();
        } else {
            panic!("WTF???");
        }
    }

    #[tokio::test]
    #[ignore]
    async fn post_stop_order() {
        // share, iid
        let share = Share::new("moex_share_vtbr").unwrap();
        let iid = share.iid();

        // connect broker
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        // get account
        let a = b.get_account("Agni").await.unwrap();

        // get last price
        let price = b.get_last_price(iid).await.unwrap();
        let price = price + 3.0;

        // create order
        let new_order = StopOrder::new(
            StopOrderKind::TakeProfit,
            Direction::Sell,
            1,
            price,
            Some(price),
        );

        // post order
        let order = b.post_stop(&a, iid, new_order.clone()).await.unwrap();

        // get stop orders
        let stop_orders = b.get_stop_orders(&a, iid).await.unwrap();
        assert!(!stop_orders.is_empty());

        // cancel order if "Posted"
        if let StopOrder::Posted(posted_order) = order {
            assert_eq!(posted_order.direction, new_order.direction);
            assert_eq!(posted_order.lots, new_order.lots);
            // b.cancel_stop(&a, posted_order).await.unwrap();
        } else {
            panic!("WTF???");
        }
    }

    #[tokio::test]
    #[ignore]
    async fn data_stream() {
        // share, iid
        let sber = Share::new("moex_share_sber").unwrap();
        let tf = TimeFrame::M1;

        // connect broker
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut b = TinkoffClient::new(event_tx);
        b.connect().await.unwrap();

        // create_marketdata_stream
        b.create_marketdata_stream().await.unwrap();

        // subscribe bar 1M
        b.subscribe_bar(sber.iid(), &tf).await.unwrap();
        b.subscribe_tic(sber.iid()).await.unwrap();

        // // create task - broker start data stream loop
        // tokio::spawn(async move { b.start().await });

        // event receiving loop
        let mut bar = 1;
        let mut tic = 1;
        while let Some(e) = event_rx.recv().await {
            match e {
                Event::Bar(e) => {
                    assert_eq!(e.figi, *sber.figi());
                    bar -= 1;
                }
                Event::Tic(e) => {
                    assert_eq!(e.figi, *sber.figi());
                    tic -= 1;
                }
                Event::Order(_) => {}
            }
            if bar <= 0 && tic <= 0 {
                break;
            }
        }
    }
}
