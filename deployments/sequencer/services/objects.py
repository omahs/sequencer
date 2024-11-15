import dataclasses

from typing import Optional, Dict, Any, Mapping, Sequence
from enum import Enum


@dataclasses.dataclass
class Probe:
    port: int | str
    path: str
    period_seconds: int
    failure_threshold: int
    timeout_seconds: int

    def __post_init__(self):
        assert not isinstance(self.port, (bool)), \
            "Port must be of type int or str, not bool."


@dataclasses.dataclass
class HealthCheck:
    startup_probe: Optional[Probe] = None
    readiness_probe: Optional[Probe] = None
    liveness_probe: Optional[Probe] = None


@dataclasses.dataclass
class ServiceType(Enum):
    CLUSTER_IP = "ClusterIP"
    LOAD_BALANCER = "LoadBalancer"
    NODE_PORT = "NodePort"


@dataclasses.dataclass
class PersistentVolumeClaim:
    storage_class_name: str | None
    access_modes: list[str] | None
    volume_mode: str | None
    storage: str | None
    read_only: bool | None
    mount_path: str | None


@dataclasses.dataclass
class Config():
    schema: Dict[Any, Any]
    config: Dict[Any, Any]
    mount_path: str

    def get(self):
        return self.config

    def validate(self):
        pass


@dataclasses.dataclass
class PortMapping:
    name: str
    port: int
    container_port: int


@dataclasses.dataclass
class IngressRuleHttpPath:
    path: Optional[str]
    path_type: str
    backend_service_name: str
    backend_service_port_number: int
    backend_service_port_name: Optional[str]


@dataclasses.dataclass
class IngressRule:
    host: str
    paths: Sequence[IngressRuleHttpPath]


@dataclasses.dataclass
class IngressTls:
    hosts: Sequence[str] | None
    secret_name: str | None


@dataclasses.dataclass
class Ingress:
    annotations: Mapping[str, str] | None
    class_name: str | None
    rules: Sequence[IngressRule] | None
    tls: Sequence[IngressTls] | None
