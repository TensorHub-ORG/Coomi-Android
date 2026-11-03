use crate::{WorkflowState, WorkflowStatus, WorkflowStep, WorkflowStepState};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::time::Instant;

/// Explicit outcome: a JSON value (including null or an "error" field) never decides success.
#[derive(Clone, Debug)]
pub struct StepExecResult {
    pub success: bool,
    pub value: Value,
    pub retryable: bool,
}
impl StepExecResult {
    pub fn success(value: impl Into<Value>) -> Self {
        Self {
            success: true,
            value: value.into(),
            retryable: false,
        }
    }
    pub fn failure() -> Self {
        Self::error("step failed")
    }
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            success: false,
            value: json!({"error":message.into()}),
            retryable: false,
        }
    }
    pub fn retryable_failure() -> Self {
        Self {
            retryable: true,
            ..Self::failure()
        }
    }
}
#[derive(Clone, Debug)]
pub struct StepContext<'a> {
    pub workflow: &'a WorkflowState,
    pub step: &'a WorkflowStep,
    pub variables: &'a BTreeMap<String, Value>,
}
#[async_trait::async_trait]
pub trait StepExecutor: Send + Sync {
    async fn execute(&self, ctx: &StepContext<'_>) -> StepExecResult;
    /// Persist before any side effect, and after every attempt.
    fn checkpoint(&self, _workflow: &WorkflowState) -> Result<()> {
        Ok(())
    }
    /// Pause only at a safe boundary, never in the middle of a script.
    async fn safe_point(&self) -> Result<()> {
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkflowRunOutcome {
    Completed,
    Failed,
    Cancelled,
}

pub struct WorkflowRunner;
impl WorkflowRunner {
    pub async fn run<E: StepExecutor>(
        workflow: &mut WorkflowState,
        executor: &E,
    ) -> Result<WorkflowRunOutcome> {
        workflow.validate().map_err(anyhow::Error::msg)?;
        workflow.reset_for_run();
        Self::resume(workflow, executor).await
    }

    /// Only explicitly selected recovery retains succeeded steps.
    pub async fn resume<E: StepExecutor>(
        workflow: &mut WorkflowState,
        executor: &E,
    ) -> Result<WorkflowRunOutcome> {
        workflow.validate().map_err(anyhow::Error::msg)?;
        for step in &mut workflow.steps {
            if step.state != WorkflowStepState::Succeeded {
                workflow.variables.remove(&step.id);
                step.state = WorkflowStepState::Pending;
                step.result = None;
            }
        }
        workflow.status = WorkflowStatus::Running;
        executor.checkpoint(workflow)?;
        loop {
            executor.safe_point().await?;
            let ready = workflow.ready_steps();
            if ready.is_empty() {
                let completed = workflow
                    .steps
                    .iter()
                    .all(|s| s.state == WorkflowStepState::Succeeded);
                workflow.status = if completed {
                    WorkflowStatus::Completed
                } else {
                    WorkflowStatus::Failed
                };
                executor.checkpoint(workflow)?;
                return Ok(if completed {
                    WorkflowRunOutcome::Completed
                } else {
                    WorkflowRunOutcome::Failed
                });
            }
            for id in ready {
                executor.safe_point().await?;
                let index = workflow
                    .steps
                    .iter()
                    .position(|s| s.id == id)
                    .expect("ready step exists");
                if !Self::execute_step(workflow, index, executor).await? {
                    workflow.status = WorkflowStatus::Failed;
                    executor.checkpoint(workflow)?;
                    return Ok(WorkflowRunOutcome::Failed);
                }
            }
        }
    }

    async fn execute_step<E: StepExecutor>(
        workflow: &mut WorkflowState,
        index: usize,
        executor: &E,
    ) -> Result<bool> {
        let attempts = workflow.steps[index].retry + 1;
        for attempt in 0..attempts {
            executor.safe_point().await?;
            workflow.steps[index].state = WorkflowStepState::Running;
            workflow.steps[index].attempts += 1;
            executor.checkpoint(workflow)?;
            let started = Instant::now();
            let result = executor
                .execute(&StepContext {
                    workflow,
                    step: &workflow.steps[index],
                    variables: &workflow.variables,
                })
                .await;
            let step = &mut workflow.steps[index];
            step.duration_ms = Some(
                step.duration_ms
                    .unwrap_or(0)
                    .saturating_add(started.elapsed().as_millis() as u64),
            );
            step.result = Some(result.value.clone());
            step.state = if result.success {
                WorkflowStepState::Succeeded
            } else {
                WorkflowStepState::Failed
            };
            if result.success {
                workflow.variables.insert(step.id.clone(), result.value);
            }
            executor.checkpoint(workflow)?;
            if result.success {
                return Ok(true);
            }
            if !result.retryable || attempt + 1 == attempts {
                return Ok(false);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StepAction;
    use crate::WorkflowStep;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    fn model_action() -> StepAction {
        StepAction::Model {
            prompt: "hi".into(),
            model: None,
            isolate: None,
        }
    }

    fn linear() -> WorkflowState {
        WorkflowState::new(
            "wf-l",
            "linear",
            vec![
                WorkflowStep::new("a", "A", model_action()),
                WorkflowStep::new("b", "B", model_action()).depends_on(&["a"]),
                WorkflowStep::new("c", "C", model_action()).depends_on(&["b"]),
            ],
        )
    }

    /// 记录每次执行的步骤 id 与调用顺序。
    struct RecordingExecutor {
        log: Arc<tokio::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl StepExecutor for RecordingExecutor {
        async fn execute(&self, ctx: &StepContext<'_>) -> StepExecResult {
            self.log.lock().await.push(ctx.step.id.clone());
            StepExecResult::success(json!({"step": ctx.step.id}))
        }
    }

    #[tokio::test]
    async fn linear_workflow_runs_in_order() {
        let mut wf = linear();
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let exec = RecordingExecutor { log: log.clone() };
        let outcome = WorkflowRunner::run(&mut wf, &exec).await.expect("run");
        assert_eq!(outcome, WorkflowRunOutcome::Completed);
        // 依赖注入：c 的 variables 包含 b 的结果
        let expected = vec!["a", "b", "c"];
        let got = log.lock().await.clone();
        assert_eq!(got, expected);
        assert_eq!(wf.status, crate::WorkflowStatus::Completed);
    }

    /// 每个步骤成功一次，但失败一次后重试成功。
    struct RetryOnceExecutor {
        counts: Arc<HashMap<String, AtomicUsize>>,
    }

    #[async_trait::async_trait]
    impl StepExecutor for RetryOnceExecutor {
        async fn execute(&self, ctx: &StepContext<'_>) -> StepExecResult {
            let counter = self.counts.get(&ctx.step.id).unwrap();
            let n = counter.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                StepExecResult::retryable_failure()
            } else {
                StepExecResult::success(json!({"ok": true}))
            }
        }
    }

    #[tokio::test]
    async fn retryable_failure_retries_then_succeeds() {
        let mut wf = linear();
        // 只有 a 需要重试；b/c 直接从第 2 次调用开始成功，避免影响其它步骤。
        wf.steps[0].retry = 2;
        wf.steps[1].retry = 1;
        wf.steps[2].retry = 1;
        let mut counts = HashMap::new();
        // a 第一次失败（n=0），第二次成功（n=1）
        counts.insert("a".to_string(), AtomicUsize::new(0));
        // b/c 第一次就成功
        counts.insert("b".to_string(), AtomicUsize::new(1));
        counts.insert("c".to_string(), AtomicUsize::new(1));
        let exec = RetryOnceExecutor {
            counts: Arc::new(counts),
        };
        let outcome = WorkflowRunner::run(&mut wf, &exec).await.expect("run");
        assert_eq!(outcome, WorkflowRunOutcome::Completed);
        assert_eq!(wf.steps[0].attempts, 2);
        assert_eq!(wf.steps[0].state, WorkflowStepState::Succeeded);
    }

    /// 一个步骤不可重试失败 → 整条 workflow Failed。
    struct AlwaysFailExecutor;
    #[async_trait::async_trait]
    impl StepExecutor for AlwaysFailExecutor {
        async fn execute(&self, _: &StepContext<'_>) -> StepExecResult {
            StepExecResult::failure()
        }
    }

    #[tokio::test]
    async fn permanent_failure_marks_workflow_failed() {
        let mut wf = linear();
        let outcome = WorkflowRunner::run(&mut wf, &AlwaysFailExecutor)
            .await
            .expect("run");
        assert_eq!(outcome, WorkflowRunOutcome::Failed);
        assert_eq!(wf.status, crate::WorkflowStatus::Failed);
        assert!(
            wf.steps
                .iter()
                .any(|s| s.state == WorkflowStepState::Failed)
        );
    }

    /// 依赖注入：把上一步产出的变量传给下一步的 ctx.
    struct InjectExecutor;
    #[async_trait::async_trait]
    impl StepExecutor for InjectExecutor {
        async fn execute(&self, ctx: &StepContext<'_>) -> StepExecResult {
            // 从 variables 读取上一步结果
            let prev = ctx.variables.get("a").cloned().unwrap_or(Value::Null);
            StepExecResult::success(json!({"prev": prev, "step": ctx.step.id}))
        }
    }

    #[tokio::test]
    async fn variables_are_shared_between_steps() {
        let mut wf = linear();
        let outcome = WorkflowRunner::run(&mut wf, &InjectExecutor)
            .await
            .expect("run");
        assert_eq!(outcome, WorkflowRunOutcome::Completed);
        // a 的结果应写入 variables
        assert_eq!(wf.variables.get("a").unwrap()["step"], "a");
        assert_eq!(wf.variables.get("b").unwrap()["prev"]["step"], "a");
    }

    /// 跳过：把失败步骤标记为 Skipped 后，其依赖者可继续。
    struct SkipOnFailExecutor;
    #[async_trait::async_trait]
    impl StepExecutor for SkipOnFailExecutor {
        async fn execute(&self, ctx: &StepContext<'_>) -> StepExecResult {
            if ctx.step.id == "a" {
                StepExecResult::failure()
            } else {
                StepExecResult::success(json!({"ok": true}))
            }
        }
    }

    #[tokio::test]
    async fn blocked_workflow_not_infinite_loop() {
        let mut wf = linear();
        // a 不可重试失败，b/c 永远无法就绪 → 不应死循环，应返回 Failed
        let outcome = WorkflowRunner::run(&mut wf, &SkipOnFailExecutor)
            .await
            .expect("run");
        assert_eq!(outcome, WorkflowRunOutcome::Failed);
    }

    struct StructuredFailure;
    #[async_trait::async_trait]
    impl StepExecutor for StructuredFailure {
        async fn execute(&self, _: &StepContext<'_>) -> StepExecResult {
            StepExecResult::error("compiler exited with code 7")
        }
    }

    #[tokio::test]
    async fn structured_failure_stops_dependencies_and_preserves_diagnostics() {
        let mut workflow = linear();
        assert_eq!(
            WorkflowRunner::run(&mut workflow, &StructuredFailure)
                .await
                .unwrap(),
            WorkflowRunOutcome::Failed
        );
        assert_eq!(
            workflow.steps[0].result.as_ref().unwrap()["error"],
            "compiler exited with code 7"
        );
        assert_eq!(workflow.steps[1].attempts, 0);
        assert!(!workflow.variables.contains_key("a"));
    }

    #[tokio::test]
    async fn new_runs_reset_all_steps_but_recovery_keeps_successful_steps() {
        let mut workflow = linear();
        workflow.variables.insert("input".into(), json!("retain"));
        let log = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let executor = RecordingExecutor { log: log.clone() };
        WorkflowRunner::run(&mut workflow, &executor).await.unwrap();
        WorkflowRunner::run(&mut workflow, &executor).await.unwrap();
        assert_eq!(log.lock().await.len(), 6);
        assert!(workflow.steps.iter().all(|step| step.attempts == 1));
        workflow.steps[2].state = WorkflowStepState::Failed;
        WorkflowRunner::resume(&mut workflow, &executor)
            .await
            .unwrap();
        assert_eq!(log.lock().await.len(), 7);
        assert_eq!(workflow.variables["input"], "retain");
    }

    struct NullSuccess;
    #[async_trait::async_trait]
    impl StepExecutor for NullSuccess {
        async fn execute(&self, _: &StepContext<'_>) -> StepExecResult {
            StepExecResult::success(Value::Null)
        }
    }

    #[tokio::test]
    async fn explicit_success_can_carry_null_data() {
        let mut workflow = linear();
        assert_eq!(
            WorkflowRunner::run(&mut workflow, &NullSuccess)
                .await
                .unwrap(),
            WorkflowRunOutcome::Completed
        );
    }

    struct BrokenCheckpoint;
    #[async_trait::async_trait]
    impl StepExecutor for BrokenCheckpoint {
        async fn execute(&self, _: &StepContext<'_>) -> StepExecResult {
            panic!("must not execute without a durable checkpoint")
        }
        fn checkpoint(&self, _: &WorkflowState) -> Result<()> {
            anyhow::bail!("disk full")
        }
    }

    #[tokio::test]
    async fn checkpoint_failure_prevents_execution() {
        assert!(
            WorkflowRunner::run(&mut linear(), &BrokenCheckpoint)
                .await
                .unwrap_err()
                .to_string()
                .contains("disk full")
        );
    }
}
