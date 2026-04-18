package sidehuddle

    // bridge.go — CGo //export bridge.
    //
    // CGo rule: a file containing //export may only have declarations (not
    // definitions) in its C preamble.  The actual C shim lives in sidehuddle.go;
    // this file only contains the exported Go function that it calls back into.

    /*
    #include "side_huddle.h"
    */
    import "C"

    import "unsafe"

    // goEventBridge is called by the C shim goEventBridgeShim in sidehuddle.go.
    // userdata is the callback registry ID cast to a pointer.
    //
    //export goEventBridge
    func goEventBridge(ev *C.SHEvent, userdata unsafe.Pointer) {
    	id := uintptr(userdata)
    	cbMu.RLock()
    	f := callbacks[id]
    	cbMu.RUnlock()
    	if f != nil {
    		f(cEventToGo(ev))
    	}
    }
    