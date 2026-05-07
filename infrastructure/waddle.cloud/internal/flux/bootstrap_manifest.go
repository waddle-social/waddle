package flux

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"time"

	kustomizev1 "github.com/fluxcd/kustomize-controller/api/v1"
	sourcev1 "github.com/fluxcd/source-controller/api/v1"
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
	"k8s.io/client-go/rest"
	"k8s.io/client-go/restmapper"
)

func applyManifest(ctx context.Context, cfg *rest.Config, manifest string) error {
	objects, err := decodeManifest(manifest)
	if err != nil {
		return err
	}
	if len(objects) == 0 {
		return fmt.Errorf("no Kubernetes objects found in Flux install manifest")
	}

	discoveryClient, err := discovery.NewDiscoveryClientForConfig(cfg)
	if err != nil {
		return fmt.Errorf("create discovery client: %w", err)
	}

	mapper := restmapper.NewDeferredDiscoveryRESTMapper(memory.NewMemCacheClient(discoveryClient))
	dynamicClient, err := dynamic.NewForConfig(cfg)
	if err != nil {
		return fmt.Errorf("create dynamic client: %w", err)
	}

	stageOne := make([]*unstructured.Unstructured, 0, len(objects))
	stageTwo := make([]*unstructured.Unstructured, 0, len(objects))

	for _, obj := range objects {
		if isClusterDefinition(obj) {
			stageOne = append(stageOne, obj)
			continue
		}
		stageTwo = append(stageTwo, obj)
	}

	if err := applyObjects(ctx, dynamicClient, mapper, stageOne); err != nil {
		return err
	}

	if err := waitForRequiredCRDs(ctx, mapper); err != nil {
		return err
	}

	if err := applyObjects(ctx, dynamicClient, mapper, stageTwo); err != nil {
		return err
	}

	return nil
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
			return nil, fmt.Errorf("decode manifest: %w", err)
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
		namespace := obj.GetNamespace()
		if namespace == "" {
			namespace = fluxNamespace
		}
		resourceClient = dynamicClient.Resource(mapping.Resource).Namespace(namespace)
	} else {
		resourceClient = dynamicClient.Resource(mapping.Resource)
	}

	force := true
	if _, err := resourceClient.Patch(ctx, obj.GetName(), types.ApplyPatchType, payload, metav1.PatchOptions{
		FieldManager: fluxFieldManager,
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

func waitForRequiredCRDs(ctx context.Context, mapper *restmapper.DeferredDiscoveryRESTMapper) error {
	required := []schema.GroupVersionKind{
		sourcev1.GroupVersion.WithKind(sourcev1.OCIRepositoryKind),
		kustomizev1.GroupVersion.WithKind(kustomizev1.KustomizationKind),
	}

	for _, gvk := range required {
		err := wait.PollUntilContextTimeout(ctx, 2*time.Second, time.Minute, true, func(context.Context) (bool, error) {
			_, err := restMapping(mapper, gvk)
			return err == nil, nil
		})
		if err != nil {
			return fmt.Errorf("wait for CRD %s: %w", gvk.String(), err)
		}
	}

	return nil
}

func isClusterDefinition(obj *unstructured.Unstructured) bool {
	if obj == nil {
		return false
	}

	gvk := obj.GroupVersionKind()
	if gvk.Kind == "Namespace" && gvk.Group == "" {
		return true
	}

	return gvk.Kind == "CustomResourceDefinition" && gvk.Group == "apiextensions.k8s.io"
}
