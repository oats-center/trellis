import {
  createInitialDeviceActivationState,
  type DeviceActivationAuth,
  type DeviceActivationClient,
  type DeviceActivationControllerConfig,
  DeviceActivationControllerCore,
  type DeviceActivationOperationRef,
} from "./device_activation_controller.ts";

export type {
  DeviceActivationAuth,
  DeviceActivationClient,
  DeviceActivationControllerConfig,
  DeviceActivationOperationRef,
} from "./device_activation_controller.ts";

export class DeviceActivationController extends DeviceActivationControllerCore {
  constructor(config: DeviceActivationControllerConfig) {
    const state = $state(createInitialDeviceActivationState());
    super(config, state);
  }
}

export function createDeviceActivationController(
  config: DeviceActivationControllerConfig,
): DeviceActivationController {
  return new DeviceActivationController(config);
}
