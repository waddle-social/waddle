import { createBrowserInspector } from '@statelyai/inspect';

let inspector: any = null;

export function initializeXStateInspector() {
  // Only initialize in development mode
  if (import.meta.env.DEV && typeof window !== 'undefined') {
    try {
      inspector = createBrowserInspector({
        // Configure the inspector
        autoStart: true,
      });
      
      console.log('🔍 XState Inspector initialized');
      console.log('Open the browser inspector to view state machines');
      
      return inspector;
    } catch (error) {
      console.warn('Failed to initialize XState Inspector:', error);
    }
  }
  
  return null;
}

export function getInspector() {
  return inspector;
}

export function inspectMachine(machine: any, options: any = {}) {
  if (inspector && import.meta.env.DEV) {
    try {
      return machine.provide({
        inspect: inspector.inspect,
        ...options,
      });
    } catch (error) {
      console.warn('Failed to inspect machine:', error);
      return machine;
    }
  }
  
  return machine;
}

// Auto-initialize in development
if (import.meta.env.DEV && typeof window !== 'undefined') {
  initializeXStateInspector();
}