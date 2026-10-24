//! Valuing Sui coins in USD for display. Failures degrade to "no value", never to an error.

use super::cetus::{self, CetusSettings};
use super::coins;
use crate::eth::prices::{native_price_url, parse_native_price};
use crate::http::HttpClient;

/// Circle's native USDC on Sui: the unit the other coins are valued in.
pub const USDC_COIN_TYPE: &str =
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC";
pub const USDC_DECIMALS: u32 = 6;
/// Valuing a coin costs one aggregator request, so only this many coins are valued per call.
pub const MAX_QUOTED_COINS: usize = 8;

pub fn sui_price_usd(http: &dyn HttpClient) -> Option<f64> {
    let body = http.get(&native_price_url("sui")).ok()?;
    parse_native_price(&body, "sui")
}

/// `raw` base units at `price` USD per whole unit.
pub fn value_usd(raw: u128, decimals: u32, price: f64) -> f64 {
    raw as f64 / 10f64.powi(decimals as i32) * price
}

/// What the Cetus aggregator would pay in USDC for the whole holding. Indicative only: it comes
/// from the same API that quotes swaps and is never used to sign anything.
pub fn quoted_value_usd(
    http: &dyn HttpClient,
    settings: &CetusSettings,
    coin_type: &str,
    raw: u128,
) -> Option<f64> {
    let amount = u64::try_from(raw).ok().filter(|a| *a > 0)?;
    let route = cetus::fetch_route(http, settings, coin_type, USDC_COIN_TYPE, amount).ok()?;
    if route.amount_in != amount || route.deviation.abs() > cetus::MAX_PRICE_DEVIATION {
        return None;
    }
    Some(value_usd(u128::from(route.amount_out), USDC_DECIMALS, 1.0))
}

/// Best available USD price for a whole holding of one coin.
pub fn holding_value_usd(
    http: &dyn HttpClient,
    settings: &CetusSettings,
    sui_price: Option<f64>,
    coin_type: &str,
    raw: u128,
    decimals: u32,
    quotes_left: &mut usize,
) -> Option<f64> {
    if coins::is_sui(coin_type) {
        return sui_price.map(|p| value_usd(raw, decimals, p));
    }
    if coin_type == USDC_COIN_TYPE {
        return Some(value_usd(raw, USDC_DECIMALS, 1.0));
    }
    if *quotes_left == 0 || raw == 0 {
        return None;
    }
    *quotes_left -= 1;
    quoted_value_usd(http, settings, coin_type, raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sui::cetus::tests::{BUCK, api_answer};
    use crate::sui::coins::SUI_COIN_TYPE;
    use crate::sui::testing::SuiMock;

    #[test]
    fn sui_is_priced_from_coingecko() {
        let ok = SuiMock::new().get("simple/price?ids=sui", r#"{"sui":{"usd":0.96}}"#);
        assert_eq!(sui_price_usd(&ok), Some(0.96));
        let down = SuiMock::new().get_error("simple/price", "boom");
        assert_eq!(sui_price_usd(&down), None);
        let junk = SuiMock::new().get("simple/price", "nope");
        assert_eq!(sui_price_usd(&junk), None);
    }

    #[test]
    fn value_math() {
        assert!((value_usd(1_500_000_000, 9, 2.0) - 3.0).abs() < 1e-9);
        assert!((value_usd(2_500_000, 6, 1.0) - 2.5).abs() < 1e-9);
        assert_eq!(value_usd(0, 9, 5.0), 0.0);
    }

    #[test]
    fn sui_and_usdc_need_no_quote() {
        let http = SuiMock::new();
        let settings = CetusSettings::default();
        let mut left = 3;
        assert_eq!(
            holding_value_usd(
                &http,
                &settings,
                Some(2.0),
                SUI_COIN_TYPE,
                1_000_000_000,
                9,
                &mut left
            ),
            Some(2.0)
        );
        assert_eq!(
            holding_value_usd(&http, &settings, None, SUI_COIN_TYPE, 1, 9, &mut left),
            None
        );
        assert_eq!(
            holding_value_usd(
                &http,
                &settings,
                None,
                USDC_COIN_TYPE,
                2_000_000,
                6,
                &mut left
            ),
            Some(2.0)
        );
        assert_eq!(left, 3);
        assert!(http.get_urls().is_empty());
    }

    #[test]
    fn other_coins_are_quoted_until_the_budget_runs_out() {
        let http = SuiMock::new().get("find_routes", &api_answer(962_476_346, 960_871));
        let settings = CetusSettings::default();
        // The mock route sells exactly 1 SUI, so value a holding of that size.
        let mut left = 1;
        let v =
            holding_value_usd(&http, &settings, None, BUCK, 1_000_000_000, 9, &mut left).unwrap();
        assert!((v - 0.960871).abs() < 1e-9);
        assert_eq!(left, 0);
        assert_eq!(
            holding_value_usd(&http, &settings, None, BUCK, 1_000_000_000, 9, &mut left),
            None
        );
        assert_eq!(http.get_urls().len(), 1);
    }

    #[test]
    fn unquotable_holdings_have_no_value() {
        let settings = CetusSettings::default();
        let none = SuiMock::new().get("find_routes", r#"{"code":5010,"msg":"no"}"#);
        assert_eq!(quoted_value_usd(&none, &settings, BUCK, 5), None);
        // A route for a different amount than asked is not trusted.
        let wrong = SuiMock::new().get("find_routes", &api_answer(962_476_346, 960_871));
        assert_eq!(quoted_value_usd(&wrong, &settings, BUCK, 5), None);
        assert_eq!(quoted_value_usd(&wrong, &settings, BUCK, 0), None);
        assert_eq!(quoted_value_usd(&wrong, &settings, BUCK, u128::MAX), None);
    }
}
