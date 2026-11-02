//! The one rs-matter handler for every endpoint the engine serves: the aggregator and the
//! product's endpoints.
//!
//! rs-matter's handler chains are compile-time types, so endpoints that come and go at run
//! time cannot be new chain links. This handler is chained once, for every endpoint but the
//! root, and dispatches by endpoint id into the [`Table`]: reads are answered from the value
//! map (Descriptor attributes are computed, global attributes come from the cluster metadata),
//! writes and commands become facade events and wait for the product's response, with a
//! timeout after which the client gets `Failure`.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use embassy_time::Timer;
use matter_rs_engine::{
    AttributeId, ClusterId, CommandId, EndpointId, Event, FabricIndex, Response, Status, Value,
};
use rs_matter::dm::{
    AsyncHandler, AttrDetails, InvokeContext, InvokeReply, MatchContext, Metadata, Node,
    ReadContext, ReadReply, Reply, WriteContext,
};
use rs_matter::error::{Error, ErrorCode};
use rs_matter::tlv::TLVWrite;
use rs_matter::utils::futures::{Either, select};

use crate::channel::Shared;
use crate::table::Table;
use crate::tlv::{ValueTlv, value_from_tlv};

/// The node metadata view over the table.
pub(crate) struct TableMetadata(pub(crate) Rc<RefCell<Table>>);

impl Metadata for TableMetadata {
    fn access<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&Node<'_>) -> R,
    {
        let table = self.0.borrow();
        f(&Node::new(table.endpoints()))
    }
}

/// The handler of the aggregator and the product's endpoints.
pub(crate) struct DynamicHandler {
    table: Rc<RefCell<Table>>,
    shared: Arc<Shared>,
    timeout: Duration,
}

impl DynamicHandler {
    /// A handler over `table`, reporting to the product through `shared`, giving the product
    /// `timeout` to answer a command or a write.
    pub(crate) fn new(table: Rc<RefCell<Table>>, shared: Arc<Shared>, timeout: Duration) -> Self {
        Self {
            table,
            shared,
            timeout,
        }
    }

    /// Waits for the product's response to `correlation`, at most for the timeout.
    pub(crate) async fn response(
        &self,
        correlation: matter_rs_engine::Correlation,
    ) -> Result<Response, Error> {
        let timeout = embassy_time::Duration::from_millis(
            u64::try_from(self.timeout.as_millis()).unwrap_or(u64::MAX),
        );
        match select(
            self.shared.await_response(correlation),
            Timer::after(timeout),
        )
        .await
        {
            Either::First(Some(response)) => Ok(response),
            Either::First(None) => {
                log::warn!("request {correlation}: the node stopped before the product answered");
                Err(ErrorCode::Failure.into())
            }
            Either::Second(()) => {
                self.shared.forget(correlation);
                log::warn!(
                    "request {correlation}: no response from the product within {:?}, answering Failure",
                    self.timeout
                );
                Err(ErrorCode::Failure.into())
            }
        }
    }
}

fn fabric_of(fab_idx: u8) -> Option<FabricIndex> {
    (fab_idx != 0).then(|| FabricIndex::new(fab_idx))
}

/// The rs-matter error that makes the Interaction Model answer with `status`.
///
/// rs-matter carries a status to the client as an error code; the codes it has cover the
/// common statuses. A status without a code reaches the client as `Failure`.
pub(crate) fn error_for_status(status: Status) -> Error {
    let code = match status {
        Status::UnsupportedAccess => ErrorCode::UnsupportedAccess,
        Status::UnsupportedEndpoint => ErrorCode::EndpointNotFound,
        Status::InvalidAction => ErrorCode::InvalidAction,
        Status::UnsupportedCommand => ErrorCode::CommandNotFound,
        Status::InvalidCommand => ErrorCode::InvalidCommand,
        Status::UnsupportedAttribute => ErrorCode::AttributeNotFound,
        Status::ConstraintError => ErrorCode::ConstraintError,
        Status::ResourceExhausted => ErrorCode::ResourceExhausted,
        Status::NotFound => ErrorCode::NotFound,
        Status::InvalidDataType => ErrorCode::InvalidDataType,
        Status::DataVersionMismatch => ErrorCode::DataVersionMismatch,
        Status::Busy => ErrorCode::Busy,
        Status::UnsupportedCluster => ErrorCode::ClusterNotFound,
        Status::NeedsTimedInteraction => ErrorCode::NeedsTimedInteraction,
        Status::UnsupportedEvent => ErrorCode::EventNotFound,
        Status::FailsafeRequired => ErrorCode::FailSafeRequired,
        Status::InvalidInState => ErrorCode::InvalidInState,
        Status::DynamicConstraintError => ErrorCode::DynamicConstraintError,
        Status::AlreadyExists => ErrorCode::AlreadyExists,
        Status::InvalidTransportType => ErrorCode::InvalidTransportType,
        _ => ErrorCode::Failure,
    };
    code.into()
}

/// Writes `value` as the reply to `attr`, honouring the array read protocol: an empty array
/// first, then one element per index until the end.
fn reply_with_value(
    attr: &AttrDetails,
    value: &Value,
    mut writer: impl Reply,
) -> Result<(), Error> {
    match attr.list_index.clone().map(|index| index.into_option()) {
        None => writer.set(ValueTlv(value)),
        Some(None) => {
            let tag = writer.tag().clone();
            {
                let mut tw = writer.writer();
                tw.start_array(&tag)?;
                tw.end_container()?;
            }
            writer.complete()
        }
        Some(Some(index)) => match value {
            Value::List(items) => match items.get(usize::from(index)) {
                Some(item) => writer.set(ValueTlv(item)),
                None => Err(ErrorCode::ConstraintError.into()),
            },
            _ => Err(ErrorCode::InvalidAction.into()),
        },
    }
}

impl AsyncHandler for DynamicHandler {
    async fn read(&self, ctx: impl ReadContext, reply: impl ReadReply) -> Result<(), Error> {
        let attr = ctx.attr();
        let (dataver, value) = {
            let table = self.table.borrow();
            let dataver = table.dataver(attr.endpoint_id, attr.cluster_id)?;
            let value = if attr.is_system() {
                None
            } else {
                Some(table.read(attr.endpoint_id, attr.cluster_id, attr.attr_id)?)
            };
            (dataver, value)
        };
        let Some(writer) = reply.with_dataver(dataver)? else {
            return Ok(());
        };
        match value {
            None => attr.cluster(ctx.metadata(), |cluster| cluster.read(attr, writer)),
            Some(value) => reply_with_value(attr, &value, writer),
        }
    }

    async fn write(&self, ctx: impl WriteContext) -> Result<(), Error> {
        let attr = ctx.attr();
        let (endpoint, cluster, attribute) = (attr.endpoint_id, attr.cluster_id, attr.attr_id);
        {
            let table = self.table.borrow();
            let dataver = table.dataver(endpoint, cluster)?;
            attr.check_dataver(dataver)?;
            if attr.is_system() {
                return Err(ErrorCode::InvalidAction.into());
            }
            table.check_write(endpoint, cluster, attribute)?;
        }
        if attr.list_index.is_some() {
            // A write of single list elements (chunked list writes) is not supported; the
            // product receives whole values only.
            return Err(ErrorCode::InvalidAction.into());
        }
        let value = value_from_tlv(ctx.data())?;
        let correlation = self.shared.issue();
        self.shared.push_event(Event::WriteRequested {
            correlation,
            endpoint: EndpointId::new(endpoint),
            cluster: ClusterId::new(cluster),
            attribute: AttributeId::new(attribute),
            value: value.clone(),
            fabric: fabric_of(attr.fab_idx),
        });
        let response = self.response(correlation).await?;
        if response.status.is_success() {
            self.table.borrow_mut().store(
                EndpointId::new(endpoint),
                ClusterId::new(cluster),
                AttributeId::new(attribute),
                value,
            );
            Ok(())
        } else {
            Err(error_for_status(response.status))
        }
    }

    async fn invoke(&self, ctx: impl InvokeContext, reply: impl InvokeReply) -> Result<(), Error> {
        let cmd = ctx.cmd();
        let (endpoint, cluster, command) = (cmd.endpoint_id, cmd.cluster_id, cmd.cmd_id);
        let response_id = self
            .table
            .borrow()
            .check_command(endpoint, cluster, command)?;
        let payload = if ctx.data().is_empty() {
            Value::Null
        } else {
            match value_from_tlv(ctx.data())? {
                Value::Struct(fields) if fields.is_empty() => Value::Null,
                other => other,
            }
        };
        let correlation = self.shared.issue();
        self.shared.push_event(Event::CommandInvoked {
            correlation,
            endpoint: EndpointId::new(endpoint),
            cluster: ClusterId::new(cluster),
            command: CommandId::new(command),
            payload,
            fabric: fabric_of(cmd.fab_idx),
        });
        let response = self.response(correlation).await?;
        if !response.status.is_success() {
            return Err(error_for_status(response.status));
        }
        match (response.payload, response_id) {
            (Some(payload), Some(response_id)) => {
                reply.with_command(response_id)?.set(ValueTlv(&payload))
            }
            (Some(_), None) => {
                log::warn!(
                    "command {command:#06X} of cluster {cluster:#06X} on endpoint {endpoint}: \
                     the response payload is dropped because the engine knows no response \
                     command id for it"
                );
                Ok(())
            }
            (None, _) => Ok(()),
        }
    }

    fn bump_dataver(&self, ctx: impl MatchContext) {
        self.table.borrow().bump(ctx.endpt(), ctx.cluster());
    }
}

#[cfg(test)]
mod tests {
    use matter_rs_engine::Status;
    use rs_matter::error::ErrorCode;

    use super::error_for_status;

    #[test]
    fn statuses_map_to_rs_matter_codes() {
        assert_eq!(
            error_for_status(Status::ConstraintError).code(),
            ErrorCode::ConstraintError
        );
        assert_eq!(
            error_for_status(Status::UnsupportedCommand).code(),
            ErrorCode::CommandNotFound
        );
        assert_eq!(error_for_status(Status::Busy).code(), ErrorCode::Busy);
        // Statuses rs-matter has no code for become a general failure.
        assert_eq!(
            error_for_status(Status::UnsupportedWrite).code(),
            ErrorCode::Failure
        );
        assert_eq!(error_for_status(Status::Timeout).code(), ErrorCode::Failure);
    }
}
