package cilium

import (
	"bytes"
	"context"
	_ "embed"
	"encoding/json"
	"fmt"
	"io"
	"strings"
	"time"

	apimeta "k8s.io/apimachinery/pkg/api/meta"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/apimachinery/pkg/types"
	"k8s.io/apimachinery/pkg/util/wait"
	utilyaml "k8s.io/apimachinery/pkg/util/yaml"
	"k8s.io/client-go/discovery"
	"k8s.io/client-go/discovery/cached/memory"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/restmapper"
	"k8s.io/client-go/tools/clientcmd"
)

const gatewayAPICRDFieldManager = "rawkode-cloud3-gateway-api"

var gatewayAPIRequiredGVKs = []schema.GroupVersionKind{
	{Group: "gateway.networking.k8s.io", Version: "v1", Kind: "GatewayClass"},
	{Group: "gateway.networking.k8s.io", Version: "v1", Kind: "Gateway"},
	{Group: "gateway.networking.k8s.io", Version: "v1", Kind: "HTTPRoute"},
	{Group: "gateway.networking.k8s.io", Version: "v1", Kind: "GRPCRoute"},
	{Group: "gateway.networking.k8s.io", Version: "v1beta1", Kind: "ReferenceGrant"},
}

//go:embed manifests/gateway-api-standard-crds.yaml
var gatewayAPIStandardCRDsManifest string

func ensureGatewayAPICRDs(ctx context.Context, kubeconfig string) error {
	restCfg, err := clientcmd.BuildConfigFromFlags("", strings.TrimSpace(kubeconfig))
	if err != nil {
		return fmt.Errorf("load kube config: %w", err)
	}

	discoveryClient, err := discovery.NewDiscoveryClientForConfig(restCfg)
	if err != nil {
		return fmt.Errorf("create discovery client: %w", err)
	}

	mapper := restmapper.NewDeferredDiscoveryRESTMapper(memory.NewMemCacheClient(discoveryClient))
	dynamicClient, err := dynamic.NewForConfig(restCfg)
	if err != nil {
		return fmt.Errorf("create dynamic client: %w", err)
	}

	objects, err := decodeManifest(gatewayAPIStandardCRDsManifest)
	if err != nil {
		return fmt.Errorf("decode gateway API CRDs manifest: %w", err)
	}

	if err := applyObjects(ctx, dynamicClient, mapper, objects); err != nil {
		return fmt.Errorf("apply gateway API CRDs: %w", err)
	}

	if err := waitForGatewayAPIResources(ctx, mapper, 2*time.Minute); err != nil {
		return fmt.Errorf("wait for gateway API CRDs: %w", err)
	}

	return nil
}

func waitForGatewayAPIResources(ctx context.Context, mapper *restmapper.DeferredDiscoveryRESTMapper, timeout time.Duration) error {
	return wait.PollUntilContextTimeout(ctx, 2*time.Second, timeout, true, func(context.Context) (bool, error) {
		for _, gvk := range gatewayAPIRequiredGVKs {
			if _, err := restMapping(mapper, gvk); err != nil {
				return false, nil
			}
		}
		return true, nil
	})
}

func decodeManifest(manifest string) ([]*unstructured.Unstructured, error) {
	decoder := utilyaml.NewYAMLOrJSONDecoder(bytes.NewReader([]byte(manifest)), 4096)
	objects := make([]*unstructured.Unstructured, 0)

	for {
		var raw map[string]interface{}
		err := decoder.Decode(&raw)
		if err == io.EOF {
			break
		}
		if err != nil {
			return nil, err
		}
		if len(raw) == 0 {
			continue
		}

		obj := &unstructured.Unstructured{Object: raw}
		if obj.GetName() == "" || obj.GetKind() == "" {
			continue
		}

		objects = append(objects, obj)
	}

	return objects, nil
}

func applyObjects(
	ctx context.Context,
	dynamicClient dynamic.Interface,
	mapper *restmapper.DeferredDiscoveryRESTMapper,
	objects []*unstructured.Unstructured,
) error {
	for _, obj := range objects {
		if err := applyObject(ctx, dynamicClient, mapper, obj); err != nil {
			return err
		}
	}

	return nil
}

func applyObject(
	ctx context.Context,
	dynamicClient dynamic.Interface,
	mapper *restmapper.DeferredDiscoveryRESTMapper,
	obj *unstructured.Unstructured,
) error {
	mapping, err := restMapping(mapper, obj.GroupVersionKind())
	if err != nil {
		return fmt.Errorf("resolve REST mapping for %s %s: %w", obj.GetKind(), obj.GetName(), err)
	}

	payload, err := json.Marshal(obj.Object)
	if err != nil {
		return fmt.Errorf("marshal object %s/%s: %w", obj.GetKind(), obj.GetName(), err)
	}

	var resourceClient dynamic.ResourceInterface
	if mapping.Scope.Name() == apimeta.RESTScopeNameNamespace {
		resourceClient = dynamicClient.Resource(mapping.Resource).Namespace(obj.GetNamespace())
	} else {
		resourceClient = dynamicClient.Resource(mapping.Resource)
	}

	force := true
	if _, err := resourceClient.Patch(ctx, obj.GetName(), types.ApplyPatchType, payload, metav1.PatchOptions{
		FieldManager: gatewayAPICRDFieldManager,
		Force:        &force,
	}); err != nil {
		return fmt.Errorf("server-side apply %s/%s: %w", obj.GetKind(), obj.GetName(), err)
	}

	return nil
}

func restMapping(mapper *restmapper.DeferredDiscoveryRESTMapper, gvk schema.GroupVersionKind) (*apimeta.RESTMapping, error) {
	mapping, err := mapper.RESTMapping(gvk.GroupKind(), gvk.Version)
	if err == nil {
		return mapping, nil
	}

	mapper.Reset()
	return mapper.RESTMapping(gvk.GroupKind(), gvk.Version)
}
