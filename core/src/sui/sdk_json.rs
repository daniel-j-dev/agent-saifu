//! Transactions in the Sui TypeScript SDK's serialized JSON form (`Transaction.serialize()`,
//! `{"version": 2, "sender": …, "inputs": […], "commands": […]}`), which dapps send over
//! WalletConnect. They carry no gas payment and may leave objects unresolved, so the wallet
//! resolves objects from the network and chooses gas itself before anything is signed.

use sui_sdk_types::{
    Address, Argument, Command, Digest, FundsWithdrawal, Input, MakeMoveVector, MergeCoins,
    MoveCall, ObjectReference, Publish, SharedInput, SplitCoins, TransferObjects, TypeTag,
    WithdrawFrom,
};

use super::coins::parse_address;
use super::gql::ObjectInfo;
use crate::crypto::base64;
use crate::json::{self, Value};

const MAX_ITEMS: usize = 2000;

/// An object input as the dapp described it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftInput {
    Ready(Input),
    /// Only the ID is known; version, digest or shared version come from the network.
    Lookup {
        id: Address,
        mutable: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub sender: Address,
    pub inputs: Vec<DraftInput>,
    pub commands: Vec<Command>,
}

impl Draft {
    pub fn uses_gas_coin(&self) -> bool {
        fn is_gas(a: &Argument) -> bool {
            matches!(a, Argument::Gas)
        }
        self.commands.iter().any(|c| match c {
            Command::MoveCall(m) => m.arguments.iter().any(is_gas),
            Command::TransferObjects(t) => t.objects.iter().any(is_gas) || is_gas(&t.address),
            Command::SplitCoins(s) => is_gas(&s.coin) || s.amounts.iter().any(is_gas),
            Command::MergeCoins(m) => is_gas(&m.coin) || m.coins_to_merge.iter().any(is_gas),
            Command::MakeMoveVector(v) => v.elements.iter().any(is_gas),
            _ => false,
        })
    }

    /// Fills in every unresolved object with `lookup`.
    pub fn resolve(
        &self,
        mut lookup: impl FnMut(&Address) -> Result<ObjectInfo, String>,
    ) -> Result<(Vec<Input>, Vec<Command>), String> {
        let mut inputs = Vec::with_capacity(self.inputs.len());
        for input in &self.inputs {
            inputs.push(match input {
                DraftInput::Ready(ready) => ready.clone(),
                DraftInput::Lookup { id, mutable } => {
                    let info = lookup(id)?;
                    match (info.initial_shared_version, info.digest) {
                        (Some(initial), _) => {
                            Input::Shared(SharedInput::new(*id, initial, *mutable))
                        }
                        (None, Some(digest)) => {
                            Input::ImmutableOrOwned(ObjectReference::new(*id, info.version, digest))
                        }
                        (None, None) => return Err(format!("object {id} has no digest")),
                    }
                }
            });
        }
        Ok((inputs, self.commands.clone()))
    }
}

fn field<'a>(v: &'a Value, key: &str) -> Result<&'a Value, String> {
    v.get(key).ok_or_else(|| format!("missing '{key}'"))
}

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    field(v, key)?
        .as_str()
        .ok_or_else(|| format!("'{key}' must be a string"))
}

fn list<'a>(v: &'a Value, key: &str) -> Result<&'a [Value], String> {
    let items = field(v, key)?
        .as_array()
        .ok_or_else(|| format!("'{key}' must be a list"))?;
    if items.len() > MAX_ITEMS {
        return Err(format!("'{key}' has too many entries"));
    }
    Ok(items)
}

/// Accepts the short forms the SDK uses for framework packages (`0x2`).
fn address(text: &str) -> Result<Address, String> {
    let digits = text.strip_prefix("0x").unwrap_or(text);
    if digits.is_empty() || digits.len() > 64 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid address".to_string());
    }
    parse_address(&format!("0x{digits:0>64}")).map_err(|e| format!("invalid address: {e}"))
}

fn index(v: &Value) -> Result<u16, String> {
    v.as_u64()
        .and_then(|n| u16::try_from(n).ok())
        .ok_or_else(|| "invalid index".to_string())
}

fn argument(v: &Value) -> Result<Argument, String> {
    if v.get("GasCoin").is_some() {
        return Ok(Argument::Gas);
    }
    if let Some(i) = v.get("Input") {
        return Ok(Argument::Input(index(i)?));
    }
    if let Some(i) = v.get("Result") {
        return Ok(Argument::Result(index(i)?));
    }
    if let Some(Value::Array(pair)) = v.get("NestedResult")
        && let [a, b] = pair.as_slice()
    {
        return Ok(Argument::NestedResult(index(a)?, index(b)?));
    }
    let shown: String = v.to_json().chars().take(200).collect();
    Err(format!("unsupported argument: {shown}"))
}

fn arguments(v: &Value, key: &str) -> Result<Vec<Argument>, String> {
    list(v, key)?.iter().map(argument).collect()
}

fn type_tag(text: &str) -> Result<TypeTag, String> {
    text.parse::<TypeTag>()
        .map_err(|_| "invalid type argument".to_string())
}

fn reference(v: &Value) -> Result<ObjectReference, String> {
    let digest: Digest = text(v, "digest")?
        .parse()
        .map_err(|_| "invalid object digest".to_string())?;
    Ok(ObjectReference::new(
        address(text(v, "objectId")?)?,
        field(v, "version")?
            .as_u64()
            .ok_or("invalid object version")?,
        digest,
    ))
}

fn input(v: &Value) -> Result<DraftInput, String> {
    if let Some(pure) = v.get("Pure") {
        let bytes = base64::decode(text(pure, "bytes")?)
            .map_err(|_| "a pure input is not valid base64".to_string())?;
        return Ok(DraftInput::Ready(Input::Pure(bytes)));
    }
    if let Some(object) = v.get("Object") {
        if let Some(owned) = object.get("ImmOrOwnedObject") {
            return Ok(DraftInput::Ready(Input::ImmutableOrOwned(reference(
                owned,
            )?)));
        }
        if let Some(shared) = object.get("SharedObject") {
            let mutable = shared
                .get("mutable")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            return Ok(DraftInput::Ready(Input::Shared(SharedInput::new(
                address(text(shared, "objectId")?)?,
                field(shared, "initialSharedVersion")?
                    .as_u64()
                    .ok_or("invalid shared version")?,
                mutable,
            ))));
        }
        return Err("unsupported object input (receiving objects are not supported)".to_string());
    }
    if let Some(object) = v.get("UnresolvedObject") {
        let id = address(text(object, "objectId")?)?;
        // The system Clock and Random objects can only be read; for others the dapp's own flag wins.
        let read_only = [Address::from_static("0x6"), Address::from_static("0x8")].contains(&id);
        let mutable = match object.get("mutable").and_then(Value::as_bool) {
            Some(flag) => flag && !read_only,
            None => !read_only,
        };
        return Ok(DraftInput::Lookup { id, mutable });
    }
    if let Some(w) = v.get("FundsWithdrawal") {
        let amount = w
            .get("reservation")
            .and_then(|r| r.get("MaxAmountU64"))
            .and_then(Value::as_u64)
            .ok_or("unsupported funds withdrawal reservation")?;
        let coin_type = w
            .get("typeArg")
            .and_then(|t| t.get("Balance"))
            .and_then(Value::as_str)
            .ok_or("unsupported funds withdrawal type")?;
        if let Some(from) = w.get("withdrawFrom")
            && from.get("Sender").is_none()
        {
            return Err("only withdrawals from the sender's own balance are supported".to_string());
        }
        return Ok(DraftInput::Ready(Input::FundsWithdrawal(
            FundsWithdrawal::new(amount, type_tag(coin_type)?, WithdrawFrom::Sender),
        )));
    }
    if v.get("UnresolvedPure").is_some() {
        return Err("a pure input has no type information (UnresolvedPure)".to_string());
    }
    let shown: String = v.to_json().chars().take(500).collect();
    Err(format!("unsupported input: {shown}"))
}

fn command(v: &Value) -> Result<Command, String> {
    if let Some(m) = v.get("MoveCall") {
        let type_arguments = list(m, "typeArguments")?
            .iter()
            .map(|t| type_tag(t.as_str().ok_or("invalid type argument")?))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Command::MoveCall(MoveCall {
            package: address(text(m, "package")?)?,
            module: sui_sdk_types::Identifier::new(text(m, "module")?)
                .map_err(|_| "invalid module name".to_string())?,
            function: sui_sdk_types::Identifier::new(text(m, "function")?)
                .map_err(|_| "invalid function name".to_string())?,
            type_arguments,
            arguments: arguments(m, "arguments")?,
        }));
    }
    if let Some(t) = v.get("TransferObjects") {
        return Ok(Command::TransferObjects(TransferObjects {
            objects: arguments(t, "objects")?,
            address: argument(field(t, "address")?)?,
        }));
    }
    if let Some(s) = v.get("SplitCoins") {
        return Ok(Command::SplitCoins(SplitCoins {
            coin: argument(field(s, "coin")?)?,
            amounts: arguments(s, "amounts")?,
        }));
    }
    if let Some(m) = v.get("MergeCoins") {
        return Ok(Command::MergeCoins(MergeCoins {
            coin: argument(field(m, "destination")?)?,
            coins_to_merge: arguments(m, "sources")?,
        }));
    }
    if let Some(m) = v.get("MakeMoveVec") {
        let type_ = match m.get("type") {
            Some(Value::String(t)) => Some(type_tag(t)?),
            _ => None,
        };
        return Ok(Command::MakeMoveVector(MakeMoveVector {
            type_,
            elements: arguments(m, "elements")?,
        }));
    }
    if let Some(p) = v.get("Publish") {
        let modules = list(p, "modules")?
            .iter()
            .map(|m| base64::decode(m.as_str().ok_or("invalid module")?))
            .collect::<Result<Vec<_>, _>>()?;
        let dependencies = list(p, "dependencies")?
            .iter()
            .map(|d| address(d.as_str().ok_or("invalid dependency")?))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(Command::Publish(Publish {
            modules,
            dependencies,
        }));
    }
    let shown: String = v.to_json().chars().take(300).collect();
    Err(format!(
        "unsupported command (package upgrades are not supported): {shown}"
    ))
}

/// Parses the serialized transaction. Gas data and expiration are ignored: the wallet chooses
/// its own gas, and a sponsor-paid transaction is refused.
pub fn parse(source: &str) -> Result<Draft, String> {
    let v = json::parse(source).map_err(|e| format!("transaction is not valid JSON: {e}"))?;
    if v.get("version").and_then(Value::as_u64) != Some(2) {
        return Err("only version 2 serialized transactions are supported".to_string());
    }
    let sender = address(text(&v, "sender")?)?;
    if let Some(Value::String(owner)) = v.get("gasData").and_then(|g| g.get("owner"))
        && address(owner)? != sender
    {
        return Err("sponsored transactions are not supported".to_string());
    }
    let inputs = list(&v, "inputs")?
        .iter()
        .map(input)
        .collect::<Result<Vec<_>, _>>()?;
    let commands = list(&v, "commands")?
        .iter()
        .map(command)
        .collect::<Result<Vec<_>, _>>()?;
    if commands.is_empty() {
        return Err("the transaction has no commands".to_string());
    }
    Ok(Draft {
        sender,
        inputs,
        commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "0x0ee53b569620c2f9608c03625d63b45321dda7869e2701032a1476d3f47c0ac0";

    fn sample() -> String {
        format!(
            r#"{{"version":2,"sender":"{OWNER}","expiration":null,
            "gasData":{{"budget":null,"price":null,"owner":null,"payment":null}},
            "inputs":[{{"Pure":{{"bytes":"6AMAAAAAAAA="}}}},
              {{"UnresolvedObject":{{"objectId":"0x6"}}}}],
            "commands":[{{"SplitCoins":{{"coin":{{"GasCoin":true}},"amounts":[{{"Input":0}}]}}}},
              {{"MoveCall":{{"package":"0x2","module":"coin","function":"zero",
                "typeArguments":["0x2::sui::SUI"],"arguments":[]}}}},
              {{"TransferObjects":{{"objects":[{{"NestedResult":[0,0]}}],"address":{{"Input":1}}}}}}]}}"#
        )
    }

    #[test]
    fn parses_the_sdk_shape() {
        let draft = parse(&sample()).unwrap();
        assert_eq!(draft.inputs.len(), 2);
        assert_eq!(draft.commands.len(), 3);
        assert!(draft.uses_gas_coin());
        assert!(matches!(draft.inputs[1], DraftInput::Lookup { .. }));
    }

    #[test]
    fn parses_address_balance_withdrawals() {
        let withdrawal = r#"{"FundsWithdrawal":{"reservation":{"MaxAmountU64":"4722855","$kind":"MaxAmountU64"},"typeArg":{"Balance":"0x2::sui::SUI","$kind":"Balance"},"withdrawFrom":{"Sender":true,"$kind":"Sender"}}}"#;
        let source = sample().replace(r#"{"UnresolvedObject":{"objectId":"0x6"}}"#, withdrawal);
        let draft = parse(&source).unwrap();
        let DraftInput::Ready(Input::FundsWithdrawal(w)) = &draft.inputs[1] else {
            panic!("not a withdrawal");
        };
        assert_eq!(w.amount(), Some(4_722_855));
        let sponsor = source.replace(r#""Sender":true"#, r#""Sponsor":true"#);
        assert!(parse(&sponsor).is_err());
    }

    #[test]
    fn resolves_shared_and_owned_objects() {
        let draft = parse(&sample()).unwrap();
        let (inputs, _) = draft
            .resolve(|_| {
                Ok(ObjectInfo {
                    version: 9,
                    digest: None,
                    initial_shared_version: Some(1),
                    type_repr: None,
                })
            })
            .unwrap();
        let Input::Shared(clock) = &inputs[1] else {
            panic!("not shared");
        };
        assert!(!clock.mutability().is_mutable(), "the Clock is read-only");
        let (inputs, _) = draft
            .resolve(|_| {
                Ok(ObjectInfo {
                    version: 9,
                    digest: Some(Digest::ZERO),
                    initial_shared_version: None,
                    type_repr: None,
                })
            })
            .unwrap();
        assert!(matches!(inputs[1], Input::ImmutableOrOwned(_)));
    }

    #[test]
    fn refuses_sponsored_and_unknown_shapes() {
        let sponsored = sample().replace(r#""owner":null"#, r#""owner":"0xabc""#);
        assert!(parse(&sponsored).unwrap_err().contains("sponsored"));
        assert!(parse("{}").is_err());
        assert!(parse("not json").is_err());
        let unknown = sample().replace("SplitCoins", "Frobnicate");
        assert!(parse(&unknown).unwrap_err().contains("unsupported command"));
    }
}
