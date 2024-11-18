import dataclasses

from typing import Optional, List, Dict, Any, Mapping, Sequence
from enum import Enum
from imports import k8s
from cdk8s import Names


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
    startup_probe: Optional[Probe] | None = None
    readiness_probe: Optional[Probe] | None = None
    liveness_probe: Optional[Probe] | None = None


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
class Config:
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
    hosts: Sequence[str] | None = None
    secret_name: str | None = None


@dataclasses.dataclass
class Ingress:
    annotations: Mapping[str, str] | None
    class_name: str | None
    rules: Sequence[IngressRule] | None
    tls: Sequence[IngressTls] | None


@ dataclasses.dataclass
class ContainerPort:
    pass


@dataclasses.dataclass
class VolumeMount:
    name: str
    mount_path: str
    read_only: bool


@dataclasses.dataclass
class VolumeType(Enum):
    CONFIG_MAP = "ConfigMap"
    PERSISTENT_VOLUME_CLAIM = "PersistentVolumeClaim"


@dataclasses.dataclass
class Volume:
    name: str
    type: VolumeType
    config_map_name: Optional[str] = None
    pvc_claim_name: Optional[str] = None
    pvc_read_only: Optional[bool] = None

    def __post_init__(self):
        """
        Automatically convert the dataclass instance into a Kubernetes Volume object.
        """
        if self.type == VolumeType.CONFIG_MAP:
            if not self.config_map_name:
                raise ValueError("ConfigMap volumes must have a 'config_map_name'.")
            self.k8s_volume = k8s.Volume(
                name=self.name,
                config_map=k8s.ConfigMapVolumeSource(name=self.config_map_name)
            )
        elif self.type == VolumeType.PERSISTENT_VOLUME_CLAIM:
            if not self.pvc_claim_name:
                raise ValueError("PersistentVolumeClaim volumes must have a 'pvc_claim_name'.")
            self.k8s_volume = k8s.Volume(
                name=self.name,
                persistent_volume_claim=k8s.PersistentVolumeClaimVolumeSource(
                    claim_name=self.pvc_claim_name,
                    read_only=self.pvc_read_only or False
                )
            )
        else:
            raise ValueError(f"Unsupported volume type: {self.type}")


@dataclasses.dataclass
class Container:
    name: str
    image: str
    args: List[str]
    ports: Sequence[ContainerPort]
    startup_probe: Optional[Probe]
    readiness_probe: Optional[Probe]
    liveness_probe: Optional[Probe]
    volume_mounts: Sequence[VolumeMount]


@dataclasses.dataclass
class Deployment:
    replicas: int
    annotations: Mapping[str, str] | None
    containers: Sequence[Container] | None
    volumes: Sequence[Volume] | None


@dataclasses.dataclass
class Statefulset:
    pass
