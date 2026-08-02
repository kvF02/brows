use crate::{
    ApplicationError, ApplicationResult, IdGenerator, PortErrorKind, ProxyConnectivityCheck,
    ProxyConnectivityTester, ProxyRepository, SecretStore, SecretValue,
};
use brows_contracts::{DnsModeDto, ProxyDraftDto, ProxyDto, ProxySaveRequest};
use brows_domain::{ProxyId, ProxySpec, SecretRef};
use std::sync::Arc;

#[derive(Debug)]
pub struct ProxyApplicationService {
    proxies: Arc<dyn ProxyRepository>,
    secrets: Arc<dyn SecretStore>,
    connectivity: Arc<dyn ProxyConnectivityTester>,
    ids: Arc<dyn IdGenerator>,
}

impl ProxyApplicationService {
    pub fn new(
        proxies: Arc<dyn ProxyRepository>,
        secrets: Arc<dyn SecretStore>,
        connectivity: Arc<dyn ProxyConnectivityTester>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self {
            proxies,
            secrets,
            connectivity,
            ids,
        }
    }

    pub fn list(&self) -> ApplicationResult<Vec<ProxyDto>> {
        Ok(self.proxies.list()?.iter().map(ProxyDto::from).collect())
    }

    pub fn save(&self, request: ProxySaveRequest) -> ApplicationResult<ProxyDto> {
        let credential_changed = request.credential.is_some();
        let existing = match request.proxy.id.clone() {
            Some(id) => Some(self.proxies.get(&id.try_into()?)?),
            None => None,
        };
        let id = existing
            .as_ref()
            .map(|value| value.id.clone())
            .unwrap_or(self.ids.next_proxy_id()?);
        let username = request
            .proxy
            .username
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if request.credential.is_some() && username.is_none() {
            return Err(ApplicationError::invalid_field(
                "credential",
                "requires a proxy username",
            ));
        }

        let old_ref = existing.as_ref().and_then(|value| value.secret_ref.clone());
        let new_ref = if username.is_none() {
            None
        } else if request.credential.is_some() {
            Some(old_ref.clone().unwrap_or(self.ids.next_secret_ref()?))
        } else {
            old_ref.clone()
        };
        if username.is_some() && new_ref.is_none() {
            return Err(ApplicationError::invalid_field(
                "credential",
                "is required for an authenticated proxy",
            ));
        }
        let proxy = to_domain(request.proxy, id, username, new_ref.clone())?;

        let old_secret = if credential_changed {
            optional_secret(self.secrets.as_ref(), old_ref.as_ref())?
        } else {
            None
        };
        if let (Some(input), Some(reference)) = (request.credential, new_ref.as_ref()) {
            self.secrets.put(
                reference,
                &SecretValue::new(input.expose().as_bytes().to_vec())?,
            )?;
        }
        if let Err(error) = self.proxies.save(&proxy) {
            if credential_changed {
                compensate_secret(self.secrets.as_ref(), new_ref.as_ref(), old_secret.as_ref());
            }
            return Err(error.into());
        }
        if old_ref != new_ref {
            if let Some(reference) = old_ref {
                self.secrets.delete(&reference)?;
            }
        }
        Ok(ProxyDto::from(&proxy))
    }

    pub fn remove(&self, id: ProxyId) -> ApplicationResult<()> {
        let proxy = self.proxies.get(&id)?;
        let saved = match (
            proxy.secret_ref.clone(),
            optional_secret(self.secrets.as_ref(), proxy.secret_ref.as_ref())?,
        ) {
            (Some(reference), Some(value)) => Some((reference, value)),
            _ => None,
        };
        if let Some((reference, _)) = &saved {
            self.secrets.delete(reference)?;
        }
        if let Err(error) = self.proxies.delete(&id) {
            if let Some((reference, value)) = &saved {
                let _ = self.secrets.put(reference, value);
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub fn test(&self, id: ProxyId) -> ApplicationResult<ProxyConnectivityCheck> {
        let proxy = self.proxies.get(&id)?;
        let credential = proxy
            .secret_ref
            .as_ref()
            .map(|reference| self.secrets.get(reference))
            .transpose()?;
        Ok(self.connectivity.test(&proxy, credential.as_ref())?)
    }
}

fn to_domain(
    draft: ProxyDraftDto,
    id: ProxyId,
    username: Option<String>,
    secret_ref: Option<SecretRef>,
) -> ApplicationResult<ProxySpec> {
    let proxy = ProxySpec {
        id,
        name: draft.name.trim().to_owned(),
        scheme: draft.scheme.into(),
        host: draft.host.trim().to_owned(),
        port: draft.port,
        username,
        secret_ref,
        dns_mode: match draft.dns_mode {
            DnsModeDto::Proxy => brows_domain::DnsMode::Proxy,
            DnsModeDto::Local => brows_domain::DnsMode::Local,
        },
        bypass_hosts: draft
            .bypass_hosts
            .into_iter()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .collect(),
        connect_timeout_ms: draft.connect_timeout_ms,
    };
    proxy.validate()?;
    Ok(proxy)
}

fn compensate_secret(
    store: &dyn SecretStore,
    reference: Option<&SecretRef>,
    old: Option<&SecretValue>,
) {
    if let Some(reference) = reference {
        if let Some(value) = old {
            let _ = store.put(reference, value);
        } else {
            let _ = store.delete(reference);
        }
    }
}

fn optional_secret(
    store: &dyn SecretStore,
    reference: Option<&SecretRef>,
) -> ApplicationResult<Option<SecretValue>> {
    let Some(reference) = reference else {
        return Ok(None);
    };
    match store.get(reference) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind == PortErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        memory::{
            InMemoryProxyConnectivityTester, InMemoryProxyRuntime, InMemorySecretStore,
            SequentialIdGenerator,
        },
        ProxyRepository, SecretStore,
    };
    use brows_contracts::{
        DnsModeDto, ProxyDraftDto, ProxySaveRequest, ProxySchemeDto, SecretInput,
    };

    fn request(credential: Option<&str>) -> ProxySaveRequest {
        ProxySaveRequest {
            proxy: ProxyDraftDto {
                id: None,
                name: "Tokyo".to_owned(),
                scheme: ProxySchemeDto::Socks5,
                host: "proxy.example.test".to_owned(),
                port: 1080,
                username: Some("tester".to_owned()),
                dns_mode: DnsModeDto::Proxy,
                bypass_hosts: vec![],
                connect_timeout_ms: 5_000,
            },
            credential: credential.map(|value| SecretInput::new(value).unwrap()),
        }
    }

    #[test]
    fn saves_secret_outside_metadata_and_never_returns_it() {
        let proxies = Arc::new(InMemoryProxyRuntime::default());
        let secrets = Arc::new(InMemorySecretStore::default());
        let service = ProxyApplicationService::new(
            proxies.clone(),
            secrets.clone(),
            Arc::new(InMemoryProxyConnectivityTester),
            Arc::new(SequentialIdGenerator::default()),
        );
        let dto = service.save(request(Some("not-in-dto"))).unwrap();
        assert!(dto.has_secret);
        assert!(!format!("{dto:?}").contains("not-in-dto"));
        let stored = proxies
            .get(&ProxyId::new(dto.id.as_str()).unwrap())
            .unwrap();
        let secret = secrets.get(stored.secret_ref.as_ref().unwrap()).unwrap();
        assert_eq!(secret.expose(), b"not-in-dto");
        assert!(service.test(stored.id.clone()).unwrap().reachable);
    }

    #[test]
    fn authenticated_proxy_requires_credential_on_create() {
        let service = ProxyApplicationService::new(
            Arc::new(InMemoryProxyRuntime::default()),
            Arc::new(InMemorySecretStore::default()),
            Arc::new(InMemoryProxyConnectivityTester),
            Arc::new(SequentialIdGenerator::default()),
        );
        assert!(service.save(request(None)).is_err());
    }
}
