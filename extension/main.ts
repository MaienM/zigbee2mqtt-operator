import stringify from 'json-stable-stringify-without-jsonify';
import type logger from 'zigbee2mqtt/dist/util/logger';
import type * as settings from 'zigbee2mqtt/dist/util/settings';
import type Extension from '/app/dist/extension/extension.js';
import utils from '/app/dist/util/utils.js';

type Settings = typeof settings;
type Logger = typeof logger;

type MQTTMessage = {
	topic: string;
	message: string;
};

type Zigbee2MQTTRequest<T> = {
	transaction?: string;
} & T;
type Zigbee2MQTTResponseOk<T> = {
	status: 'ok';
	data: T;
	transaction?: string;
};
type Zigbee2MQTTResponseError = {
	status: 'error';
	data: Record<string, never>;
	error: string;
	transaction?: string;
};
type Zigbee2MQTTResponse<T> = Zigbee2MQTTResponseOk<T> | Zigbee2MQTTResponseError;

interface DeviceOrGroup {
	ID: number;
	options: Record<string, unknown>;
}
type UnsetOptionsRequest = Zigbee2MQTTRequest<{
	id: string;
	paths: string[];
}>;
type UnsetOptionsResponse = Zigbee2MQTTResponse<{
	restart_required: boolean;
}>;

type GetResponse = <T>(request: unknown, data: T, error?: string) => Zigbee2MQTTResponse<T>;
const getResponse = utils.getResponse as GetResponse;

class OperatorExtension {
	private readonly zigbee: Extension['zigbee'];
	private readonly mqtt: Extension['mqtt'];
	private readonly eventBus: Extension['eventBus'];
	private readonly settings: Settings;
	private readonly logger: Logger;

	private readonly REQUEST_REGEX: RegExp;
	private readonly REQUESTS: Record<string, (message: string) => Promise<Zigbee2MQTTResponse<unknown>>> = {
		'device/unset-options': this.deviceUnsetOptions,
		'group/unset-options': this.groupUnsetOptions,
	};
	private readonly DEFAULT_EXCLUDE = ['friendlyName', 'friendly_name', 'ID', 'type', 'devices'];

	private restartRequired = false;

	constructor(
		zigbee: Extension['zigbee'],
		mqtt: Extension['mqtt'],
		_state: Extension['state'],
		_publishEntityState: Extension['publishEntityState'],
		eventBus: Extension['eventBus'],
		_enableDisableExtension: Extension['enableDisableExtension'],
		_restartCallback: Extension['restartCallback'],
		_addExtension: Extension['addExtension'],
		settings: Settings,
		logger: Logger,
	) {
		this.zigbee = zigbee;
		this.mqtt = mqtt;
		this.eventBus = eventBus;
		this.settings = settings;
		this.logger = logger;

		this.REQUEST_REGEX = new RegExp(`${settings.get().mqtt.base_topic}/bridge/request/(.*)`);

		this.onMQTTMessage = this.onMQTTMessage.bind(this);
		this.deviceUnsetOptions = this.deviceUnsetOptions.bind(this);
		this.groupUnsetOptions = this.groupUnsetOptions.bind(this);
	}

	async start(): Promise<void> {
		this.eventBus.onMQTTMessage(this, this.onMQTTMessage);
	}

	async onMQTTMessage(data: MQTTMessage): Promise<void> {
		const match = data.topic.match(this.REQUEST_REGEX);
		if (!match) {
			return;
		}

		const key = match[1].toLowerCase();
		if (key in this.REQUESTS) {
			try {
				const response = await this.REQUESTS[key](data.message);
				await this.mqtt.publish(`bridge/response/${match[1]}`, stringify(response));
			} catch (error) {
				this.logger.error(`Request '${data.topic}' failed with error: '${(error as Error).message}'`);
				this.logger.debug((error as Error).stack!);
				const response = utils.getResponse(data.message, {}, (error as Error).message);
				await this.mqtt.publish(`bridge/response/${match[1]}`, stringify(response));
			}
		}
	}

	async deviceUnsetOptions(message: string): Promise<UnsetOptionsResponse> {
		return await this.unsetEntityOptions('device', message);
	}

	async groupUnsetOptions(message: string): Promise<UnsetOptionsResponse> {
		return await this.unsetEntityOptions('group', message);
	}

	async unsetEntityOptions<T extends 'device' | 'group'>(
		entityType: T,
		message: string,
	): Promise<UnsetOptionsResponse> {
		const request = utils.parseJSON(message, '') as UnsetOptionsRequest | string;
		if (typeof request === 'string' || request.id === undefined || request.paths === undefined) {
			throw new Error('Invalid payload');
		}

		// Get the current options for the entity & remove the marked fields.
		const entity = this.getEntity(entityType, request.id);
		const id = `${entity.ID}`;
		const oldOptions = this.deepCopyWithExclusions(entity.options, this.DEFAULT_EXCLUDE);
		const options = this.deepCopyWithExclusions(entity.options, request.paths);

		// We cannot use `settings.changeEntityOptions` or `settings.apply` as these merge the data we provide with the
		// existing data, which will cause the paths we removed to not actually be unset. This means we need to use
		// `settings.set`, but unfortunately this doesn't do any validations. To mitigate this we will call
		// `settings.changeEntityOptions` after the set to have this run its validations.
		this.settings.set([`${entityType}s`, id], options);
		this.restartRequired ||= this.settings.changeEntityOptions(id, this.deepCopyWithExclusions(options, this.DEFAULT_EXCLUDE));
		this.logger.info(`Changed config for ${entityType} ${id}`);

		// Get the set of options that the normal options endpoint would return.
		const newOptions = this.deepCopyWithExclusions(entity.options, this.DEFAULT_EXCLUDE);

		// Emit result to eventbus and MQTT.
		this.eventBus.emitEntityOptionsChanged({
			from: oldOptions,
			to: newOptions,
			entity,
		});
		return getResponse(request, {
			from: oldOptions,
			to: newOptions,
			id,
			restart_required: this.restartRequired,
		});
	}

	private getEntity(type: 'group' | 'device', ID: string): DeviceOrGroup {
		const entity = this.zigbee.resolveEntity(ID);
		if (!entity || entity.constructor.name.toLowerCase() !== type) {
			throw new Error(`${utils.capitalize(type)} '${ID}' does not exist`);
		}
		return entity;
	}

	private deepCopyWithExclusions(
		data: object,
		exclude: string[],
		parentPath: string = '',
	): Record<string, unknown> {
		const result: Record<string, unknown> = {};
		for (const [key, value] of Object.entries(data)) {
			const path = `${parentPath}${key}`;
			if (exclude.find((e) => e === path) !== undefined) {
				continue;
			}

			if (typeof value === 'object' && value !== null) {
				result[key] = this.deepCopyWithExclusions(value, exclude, `${path}.`);
			} else {
				result[key] = value;
			}
		}
		return result;
	}
}

export default OperatorExtension;
