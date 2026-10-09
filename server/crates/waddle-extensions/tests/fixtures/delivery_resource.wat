(component
  (type $ty-waddle:extension/types@3.0.0 (;0;)
    (instance
      (type (;0;) (enum "denied" "invalid-request" "not-found" "unsupported" "temporary-failure"))
      (export (;1;) "host-tool-error-code" (type (eq 0)))
      (type (;2;) (record (field "value" string)))
      (export (;3;) "display-text" (type (eq 2)))
      (type (;4;) (record (field "code" 1) (field "message" 3)))
      (export (;5;) "host-tool-error" (type (eq 4)))
    )
  )
  (import "waddle:extension/types@3.0.0" (instance $waddle:extension/types@3.0.0 (;0;) (type $ty-waddle:extension/types@3.0.0)))
  (alias export $waddle:extension/types@3.0.0 "host-tool-error" (type $host-tool-error (;1;)))
  (type $ty-waddle:extension/delivery@3.0.0 (;2;)
    (instance
      (export (;0;) "delivery-key" (type (sub resource)))
      (alias outer 1 $host-tool-error (type (;1;)))
      (export (;2;) "host-tool-error" (type (eq 1)))
      (type (;3;) (borrow 0))
      (type (;4;) (result (error 2)))
      (type (;5;) (func (param "self" 3) (result 4)))
      (export (;0;) "[method]delivery-key.validate" (func (type 5)))
    )
  )
  (import "waddle:extension/delivery@3.0.0" (instance $waddle:extension/delivery@3.0.0 (;1;) (type $ty-waddle:extension/delivery@3.0.0)))
  (alias export $waddle:extension/delivery@3.0.0 "delivery-key" (type $delivery-key (;3;)))
  (import "delivery-key" (type $"#type4 delivery-key" (@name "delivery-key") (;4;) (eq $delivery-key)))
  (core module $main (;0;)
    (type (;0;) (func (param i32 i32)))
    (type (;1;) (func (param i32)))
    (type (;2;) (func (param i32 i32 i32 i32) (result i32)))
    (type (;3;) (func (param i32) (result i32)))
    (type (;4;) (func (result i32)))
    (import "cm32p2|waddle:extension/delivery@3" "[method]delivery-key.validate" (func $validate (;0;) (type 0) (param i32 i32)))
    (import "cm32p2|waddle:extension/delivery@3" "delivery-key_drop" (func $drop (;1;) (type 1) (param i32)))
    (memory (;0;) 1)
    (global $retained (;0;) (mut i32) i32.const -1)
    (export "cm32p2_memory" (memory 0))
    (export "cm32p2_realloc" (func 2))
    (export "cm32p2||check" (func 3))
    (export "cm32p2||reuse" (func 4))
    (export "cm32p2||forge" (func 5))
    (func (;2;) (type 2) (param i32 i32 i32 i32) (result i32)
      i32.const 1024
    )
    (func (;3;) (type 3) (param i32) (result i32)
      (local $valid i32)
      local.get 0
      global.set $retained
      local.get 0
      i32.const 0
      call $validate
      i32.const 0
      i32.load
      i32.eqz
      local.set $valid
      local.get 0
      call $drop
      local.get $valid
    )
    (func (;4;) (type 4) (result i32)
      global.get $retained
      i32.const 0
      call $validate
      i32.const 0
      i32.load
      i32.eqz
    )
    (func (;5;) (type 4) (result i32)
      i32.const 2147483647
      i32.const 0
      call $validate
      i32.const 0
      i32.load
      i32.eqz
    )
    (@producers
      (processed-by "wit-component" "0.261.0")
    )
  )
  (core module $wit-component-shim-module (;1;)
    (type (;0;) (func (param i32 i32)))
    (table (;0;) 1 1 funcref)
    (export "0" (func $"indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate"))
    (export "$imports" (table 0))
    (func $"indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate" (;0;) (type 0) (param i32 i32)
      local.get 0
      local.get 1
      i32.const 0
      call_indirect (type 0)
    )
    (@producers
      (processed-by "wit-component" "0.261.0")
    )
  )
  (core instance $wit-component-shim-instance (;0;) (instantiate $wit-component-shim-module))
  (alias core export $wit-component-shim-instance "0" (core func $"indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate" (;0;)))
  (alias export $waddle:extension/delivery@3.0.0 "delivery-key" (type $"#type5 delivery-key" (@name "delivery-key") (;5;)))
  (core func $resource.drop (;1;) (canon resource.drop $"#type5 delivery-key"))
  (core instance $cm32p2|waddle:extension/delivery@3 (;1;)
    (export "[method]delivery-key.validate" (func $"indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate"))
    (export "delivery-key_drop" (func $resource.drop))
  )
  (core instance $main (;2;) (instantiate $main
      (with "cm32p2|waddle:extension/delivery@3" (instance $cm32p2|waddle:extension/delivery@3))
    )
  )
  (alias core export $main "cm32p2_memory" (core memory $memory (;0;)))
  (core module $wit-component-fixup (;2;)
    (type (;0;) (func (param i32 i32)))
    (import "actual" "0" (func $0 (;0;) (type 0) (param i32 i32)))
    (import "shim" "$imports" (table (;0;) 1 1 funcref))
    (elem (;0;) (i32.const 0) func $0)
    (@producers
      (processed-by "wit-component" "0.261.0")
    )
  )
  (alias export $waddle:extension/delivery@3.0.0 "[method]delivery-key.validate" (func $"[method]delivery-key.validate" (;0;)))
  (alias core export $main "cm32p2_realloc" (core func $cm32p2_realloc (;2;)))
  (core func $"#core-func3 indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate" (@name "indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate") (;3;) (canon lower (func $"[method]delivery-key.validate") (memory $memory) (realloc $cm32p2_realloc) string-encoding=utf8))
  (core instance $actual (;3;)
    (export "0" (func $"#core-func3 indirect-cm32p2|waddle:extension/delivery@3-[method]delivery-key.validate"))
  )
  (core instance $fixup (;4;) (instantiate $wit-component-fixup
      (with "actual" (instance $actual))
      (with "shim" (instance $wit-component-shim-instance))
    )
  )
  (type (;6;) (borrow $"#type4 delivery-key"))
  (type (;7;) (func (param "key" 6) (result bool)))
  (alias core export $main "cm32p2||check" (core func $cm32p2||check (;4;)))
  (func $check (;1;) (type 7) (canon lift (core func $cm32p2||check)))
  (export $"#func2 check" (@name "check") (;2;) "check" (func $check))
  (type (;8;) (func (result bool)))
  (alias core export $main "cm32p2||reuse" (core func $cm32p2||reuse (;5;)))
  (func $reuse (;3;) (type 8) (canon lift (core func $cm32p2||reuse)))
  (export $"#func4 reuse" (@name "reuse") (;4;) "reuse" (func $reuse))
  (alias core export $main "cm32p2||forge" (core func $cm32p2||forge (;6;)))
  (func $forge (;5;) (type 8) (canon lift (core func $cm32p2||forge)))
  (export $"#func6 forge" (@name "forge") (;6;) "forge" (func $forge))
  (@producers
    (processed-by "wit-component" "0.261.0")
  )
)
