use super::error::Result;
use crate::clients::crd_client::JsonPatch;
use crate::clients::CrdClient;
use crate::{AgentStatus, TaskState, Test, TestResults, TestStatus};
use kube::Api;

/// An API Client for TestSys Test CRD objects.
///
/// # Example
///
/// ```
///# use model::clients::{CrdClient, TestClient};
///# async fn no_run() {
/// let test_client = TestClient::new().await.unwrap();
/// let test = test_client.get("my-test").await.unwrap();
///# }
/// ```
#[derive(Clone)]
pub struct TestClient {
    api: Api<Test>,
}

impl TestClient {
    /// Mark the TestSys [`Test`] as ok to delete by setting the `keep_running`
    /// flag to false
    pub async fn send_keep_running<S>(&self, name: S, keep_running: bool) -> Result<Test>
    where
        S: AsRef<str> + Send,
    {
        self.patch(
            name,
            vec![JsonPatch::new_replace_operation(
                "/spec/agent/keep_running",
                keep_running,
            )],
            "set 'keep running'",
        )
        .await
    }

    /// Get the TestSys [`Test`]'s `status.agent` field.
    pub async fn get_agent_status<S>(&self, name: S) -> Result<AgentStatus>
    where
        S: AsRef<str> + Send,
    {
        Ok(self
            .get(name)
            .await?
            .status
            .unwrap_or_else(Default::default)
            .agent)
    }

    pub async fn send_resource_error(&self, test_name: &str, error: &str) -> Result<Test> {
        self.patch_status(
            test_name,
            vec![JsonPatch::new_add_operation(
                "/status/controller/resource_error",
                error,
            )],
            "send resource error",
        )
        .await
    }

    pub async fn send_agent_task_state(&self, name: &str, task_state: TaskState) -> Result<Test> {
        self.patch_status(
            name,
            vec![JsonPatch::new_add_operation(
                "/status/agent/task_state",
                task_state,
            )],
            "send agent task state",
        )
        .await
    }

    pub async fn send_test_completed(&self, name: &str, results: TestResults) -> Result<Test> {
        self.patch_status(
            name,
            vec![
                JsonPatch::new_add_operation("/status/agent/task_state", TaskState::Completed),
                JsonPatch::new_add_operation("/status/agent/results", results),
            ],
            "send test completion results",
        )
        .await
    }

    pub async fn send_agent_error(&self, name: &str, error: &str) -> Result<Test> {
        self.patch_status(
            name,
            vec![
                JsonPatch::new_add_operation("/status/agent/task_state", TaskState::Error),
                JsonPatch::new_add_operation("/status/agent/error", error),
            ],
            "send agent error",
        )
        .await
    }
}

impl CrdClient for TestClient {
    type Crd = Test;
    type CrdStatus = TestStatus;

    fn new_from_api(api: Api<Self::Crd>) -> Self {
        Self { api }
    }

    fn kind(&self) -> &'static str {
        "test"
    }

    fn api(&self) -> &Api<Self::Crd> {
        &self.api
    }
}

#[cfg(test)]
#[cfg(feature = "integ")]
mod test {
    use super::*;
    use crate::{Agent, Configuration, TestSpec};
    use k8s_openapi::api::core::v1::Namespace;
    use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use kube::api::PostParams;
    use kube::core::object::HasStatus;
    use kube::CustomResourceExt;
    use selftest::Cluster;
    use serde::{Deserialize, Serialize};
    use std::fmt::Debug;

    const CLUSTER_NAME: &str = "test-client";
    const TEST_NAME: &str = "my-test";

    #[derive(Default, Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
    struct TestConfig {
        field_a: u64,
        field_b: u64,
    }

    impl Configuration for TestConfig {}

    const TEST_CONFIG: TestConfig = TestConfig {
        field_a: 13,
        field_b: 14,
    };

    #[tokio::test]
    async fn test() {
        let cluster = Cluster::new(CLUSTER_NAME).unwrap();
        let k8s_client = cluster.k8s_client().await.unwrap();
        let ns_api: Api<Namespace> = Api::all(k8s_client.clone());
        ns_api
            .create(&PostParams::default(), &crate::system::testsys_namespace())
            .await
            .unwrap();
        let crd_api: Api<CustomResourceDefinition> = Api::all(k8s_client.clone());
        crd_api
            .create(&PostParams::default(), &Test::crd())
            .await
            .unwrap();
        let tc = TestClient::new_from_k8s_client(cluster.k8s_client().await.unwrap());

        tc.create(Test {
            metadata: ObjectMeta {
                name: Some(TEST_NAME.into()),
                ..ObjectMeta::default()
            },
            spec: TestSpec {
                agent: Agent {
                    name: "my-agent".into(),
                    image: "foo:v0.1.0".into(),
                    configuration: Some(TEST_CONFIG.into_map().unwrap()),
                    ..Agent::default()
                },
                ..TestSpec::default()
            },
            ..Test::default()
        })
        .await
        .unwrap();

        tc.initialize_status(TEST_NAME).await.unwrap();

        // If status is already initialized, it should be an error to do so again.
        assert!(tc.initialize_status(TEST_NAME).await.is_err());

        tc.send_agent_task_state(TEST_NAME, TaskState::Error)
            .await
            .unwrap();
        assert!(matches!(
            tc.get(TEST_NAME).await.unwrap().agent_status().task_state,
            TaskState::Error
        ));

        tc.send_agent_task_state(TEST_NAME, TaskState::Running)
            .await
            .unwrap();
        assert!(matches!(
            tc.get(TEST_NAME).await.unwrap().agent_status().task_state,
            TaskState::Running
        ));

        tc.send_resource_error(TEST_NAME, "something bad happened")
            .await
            .unwrap();
        assert_eq!(
            tc.get(TEST_NAME)
                .await
                .unwrap()
                .status()
                .cloned()
                .unwrap()
                .controller
                .resource_error
                .unwrap(),
            "something bad happened"
        );

        tc.send_agent_error(TEST_NAME, "something terrible happened")
            .await
            .unwrap();
        assert_eq!(
            tc.get(TEST_NAME)
                .await
                .unwrap()
                .status()
                .cloned()
                .unwrap()
                .agent
                .error
                .unwrap(),
            "something terrible happened"
        );
        assert!(matches!(
            tc.get(TEST_NAME).await.unwrap().agent_status().task_state,
            TaskState::Error
        ));
    }
}