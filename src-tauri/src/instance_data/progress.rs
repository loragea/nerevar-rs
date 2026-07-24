use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::reporter::{emit_event, EventSink};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub enum BackgroundOperationPhase {
    ScanningPackages,
    MergingLoadOrder,
    SavingLoadOrder,
    ParsingCsv,
    ApplyingLoadOrder,
    LoadingLoadOrder,
    ResolvingLoadOrder,
    UpdatingServerMetadata,
    HashingPackage,
    HashingFiles,
    WritingManifest,
    WritingLaunchCfg,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct BackgroundOperationProgressEvent {
    pub operation_id: String,
    pub instance_id: String,
    pub phase: BackgroundOperationPhase,
    pub message: String,
    pub step: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_item: Option<String>,
}

pub struct ProgressEmitter {
    sink: Arc<dyn EventSink>,
    instance_id: String,
    operation_id: String,
    last_emit: Instant,
}

impl ProgressEmitter {
    pub fn new(sink: Arc<dyn EventSink>, instance_id: String, operation_id: String) -> Self {
        Self {
            sink,
            instance_id,
            operation_id,
            last_emit: Instant::now() - Duration::from_secs(1),
        }
    }

    pub fn emit(
        &mut self,
        phase: BackgroundOperationPhase,
        message: impl Into<String>,
        step: u64,
        total: u64,
        current_item: Option<String>,
        force: bool,
    ) {
        if !force && self.should_throttle(phase) {
            return;
        }

        self.last_emit = Instant::now();
        emit_event(
            &*self.sink,
            "background-operation-progress",
            &BackgroundOperationProgressEvent {
                operation_id: self.operation_id.clone(),
                instance_id: self.instance_id.clone(),
                phase,
                message: message.into(),
                step,
                total,
                current_item,
            },
        );
    }

    fn should_throttle(&self, phase: BackgroundOperationPhase) -> bool {
        if !matches!(
            phase,
            BackgroundOperationPhase::HashingFiles | BackgroundOperationPhase::ScanningPackages
        ) {
            return false;
        }
        self.last_emit.elapsed() < Duration::from_millis(100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::CollectingEventSink;

    #[test]
    fn emit_forced_sends_events_to_sink() {
        let sink = Arc::new(CollectingEventSink::default());
        let mut emitter = ProgressEmitter::new(
            sink.clone(),
            "instance-1".to_string(),
            "operation-1".to_string(),
        );

        emitter.emit(
            BackgroundOperationPhase::ScanningPackages,
            "Scanning packages",
            0,
            2,
            None,
            true,
        );
        emitter.emit(
            BackgroundOperationPhase::ScanningPackages,
            "Scanning packages",
            1,
            2,
            Some("mod.esp".to_string()),
            true,
        );

        let events = sink.events();
        assert_eq!(events.len(), 2);
        for (name, _) in &events {
            assert_eq!(*name, "background-operation-progress");
        }

        assert_eq!(events[0].1["instanceId"], "instance-1");
        assert_eq!(events[0].1["operationId"], "operation-1");
        assert_eq!(events[0].1["step"], 0);
        assert_eq!(events[0].1["total"], 2);

        assert_eq!(events[1].1["instanceId"], "instance-1");
        assert_eq!(events[1].1["operationId"], "operation-1");
        assert_eq!(events[1].1["step"], 1);
        assert_eq!(events[1].1["currentItem"], "mod.esp");
    }
}
