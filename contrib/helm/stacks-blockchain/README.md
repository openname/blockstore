# Stacks Blockchain

[stacks-blockchain](https://github.com/blockstack/stacks-blockchain) is a layer-1 blockchain that connects to Bitcoin for security and enables decentralized apps and predictable smart contracts.

## TL;DR

```bash
$ helm repo add blockstack https://charts.blockstack.xyz
$ helm install my-release blockstack/stacks-blockchain
```

## Introduction

This chart bootstraps a [stacks-blockchain](https://github.com/blockstack/stacks-blockchain) deployment on a [Kubernetes](http://kubernetes.io) cluster using the [Helm](https://helm.sh) package manager.

## Prerequisites

- Kubernetes 1.12+
- Helm 2.12+ or Helm 3.0-beta3+

## Installing the Chart

To install the chart with the release name `my-release` and run the node as a **follower**:

```bash
$ helm repo add blockstack https://charts.blockstack.xyz
$ helm install my-release blockstack/stacks-blockchain
```

To install the chart with the release name `my-release` and run the node as a **miner** using your private key [generated from the instructions on this page](https://docs.stacks.co/docs/nodes-and-miners/miner-mainnet):

```bash
$ helm repo add blockstack https://charts.blockstack.xyz
$ helm install my-release blockstack/stacks-blockchain --set config.node.miner=true --set config.node.seed="REPLACE-WITH-YOUR-PRIVATE-KEY"
```

These commands deploy the Stacks Blockchain node on a Kubernetes cluster in the default configuration.

> **Tip**: List all releases using `helm list`

## Uninstalling the Chart

To uninstall/delete the `my-release` deployment:

```bash
$ helm delete my-release
```

The command removes all the Kubernetes components associated with the chart and deletes the release.

## Parameters

The following tables lists the configurable parameters of the stacks-blockchain chart and their default values.

| Parameter | Description | Default |
|-|-|-|
| nameOverride | String to partially override stacks-blockchain.fullname template (will maintain the release name) | nil |
| fullnameOverride | String to fully override stacks-blockchain.fullname template | nil |
| node.image.repository | stacks-blockchain container image repository | blockstack/stacks-blockchain |
| node.image.pullPolicy | stacks-blockchain container image pull policy | IfNotPresent |
| node.image.tag | stacks-blockchain container image tag | latest |
| node.imagePullSecrets | Docker registry secret names as an array | [] |
| node.replicaCount | Number of stacks-blockchain nodes to run | 1 |
| node.rpcPort | Port used for RPC connections | 20443 |
| node.p2pPort | Port used for P2P connections | 20444 |
| node.labels | Labels to be added to the node Deployment | {} |
| node.annotations | Annotations to be added to the node Deployment | {} |
| node.podAnnotations | Annotations to be added to the node pod(s) | {} |
| node.podAnnotationConfigChecksum | Add Config Checksum to pod Annotations.<br>This will trigger Rolling Updates on configuration changes. | false |
| node.podSecurityContext | Security Context policies for the node pod(s) | {} |
| node.securityContext | Security Context policies for the node container | {} |
| node.debug | Set to true for verbose logging. Set to false for normal verbosity | false |
| node.jsonLogging | Set to true to enable json logging. Set to false for normal logging | false |
| node.command | Overriding this is typically not necessary, unless you wish to run the node in a different way | ["/bin/stacks-node"] |
| node.args | Overriding this is typically not necessary, unless you wish to run the node in a different way | ["start", "--config", "/src/stacks-node/config.toml"] |
| node.extraEnv | Extra environment variables to add to the node's container | [] |
| node.resources | Specify resource limits and requests for the node's container | {} |
| node.revisionHistoryLimit | Rollback limit | 10 |
| node.updateStrategy | The update strategy to apply | {} |
| node.minReadySeconds | minReadySeconds to avoid killing pods before it's ready | 0 |
| node.nodeSelector | Node labels for node pod assignment | {} |
| node.tolerations | Node tolerations for server scheduling to nodes with taints | {} |
| node.affinity | Affinity and anti-affinity | {} |
| node.terminationGracePeriodSeconds | Total time it takes for the Container to stop normally | 60 |
| node.volumes | Additional volumes for the node | [] |
| node.volumeMounts | Additional volumeMounts for the node | [] |
| node.extraContainers | Additional containers to run alongside the node. Useful if adding a sidecar | [] |
| node.initContainers | Containers which are run before the node container is started | [] |
| config | More configs can be added than what's shown below.All children fields under the node, burnchain, and ustx_balance fields will be converted from YAML to valid TOML format in the Configmap.<br><br>For info on more available config fields, please reference to our [example config files located here](https://github.com/blockstack/stacks-blockchain/tree/master/sample/conf). |  |
| config.node.rpc_bind |  | 0.0.0.0:20443 |
| config.node.p2p_bind |  | 0.0.0.0:20444 |
| config.node.seed | Replace with your private key if deploying a miner node | nil |
| config.node.miner | Set this to `true` if deploying a miner node.<br>Set this to `false` if deploying a follower node. | false |
| config.burnchain.chain |  | bitcoin |
| config.burnchain.mode |  | krypton |
| config.burnchain.peer_host |  | bitcoin.mainnet.stacks.org |
| config.burnchain.rpc_port |  | 18443 |
| config.burnchain.peer_port |  | 18444 |
| config.ustx_balance |  | See values.yaml |
| config.raw | Uncommenting this block will give you greater control over the settings in the Configmap | nil |
| config.annotations | Annotations to be added to the Configmap | {} |
| service.type | The service type to create.<br>If creating a public node for others to connect to, use Loadbalancer to ensure the advertised IP is reachable. | ClusterIP |
| service.externalTrafficPolicy | Set external traffic policy to "Local" to preserve source IP on providers supporting it.<br>This field is only used when service.type is set to "NodePort" or "LoadBalancer".<br>Setting to "Local" can help the Stacks network if using service type NodePort or LoadBalancer. | Local |
| service.rpcPort | Will use node.rpcPort if omitted | nil |
| service.p2pPort | Will use node.p2pPort if omitted | nil |
| service.annotations | Annotations to be added to the Service | {} |
| serviceAccount.create | Specifies whether a ServiceAccount should be created.<br>If true, the stacks-blockchain node will be ran using this ServiceAccount. | true |
| serviceAccount.annotations | Annotations to add to the service account | {} |
| serviceAccount.name | The name of the service account to use.<br>If not set and create is true, a name is generated using the fullname template. | nil |
| rbac.create | Specifies whether a Role should be created | false |
| rbac.rules | Rules to add to the Role resource bound to the ServiceAccount | [] |
| metrics.enabled | Specifies whether a ServiceMonitor should be created | false |
| metrics.port | Port used by the ServiceMonitor to collect metrics from the node | 9153 |
| metrics.interval | Interval at which metrics should be collected | 30s |

Specify each parameter using the `--set key=value[,key=value]` argument to `helm install`. For example,

```bash
$ helm install my-release \
    --set node.image.pullPolicy=Always \
    blockstack/stacks-blockchain
```

The above command sets the `node.image.pullPolicy` to `Always`.

Alternatively, a YAML file that specifies the values for the parameters can be provided while installing the chart. For example,

```bash
$ helm install my-release -f values.yaml blockstack/stacks-blockchain
```

> **Tip**: You can use the default [values.yaml](values.yaml)

## Troubleshooting

If you encounter an issue with this Helm chart, please submit an issue to this Github repo [here](https://github.com/blockstack/stacks-blockchain/issues).
