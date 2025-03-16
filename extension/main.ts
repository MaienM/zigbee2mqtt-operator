import bind from 'bind-decorator';
import stringify from 'json-stable-stringify-without-jsonify';
import type logger from 'zigbee2mqtt/dist/util/logger';
import type * as settings from 'zigbee2mqtt/dist/util/settings';
import Extension from '/app/dist/extension/extension';
import utils from '/app/dist/util/utils';

type Settings = typeof settings;
type Logger = typeof logger;

type MQTTMessage = {
	topic: string;
	message: string;
};

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

class OperatorExtension extends Extension {
	private readonly REQUEST_REGEX: RegExp;
	private readonly REQUESTS: Record<string, (message: string) => Promise<Zigbee2MQTTResponse<unknown>>> = {};

	private readonly settings: Settings;
	private readonly logger: Logger;

	private restartRequired = false;

	constructor(
		zigbee: Extension['zigbee'],
		mqtt: Extension['mqtt'],
		state: Extension['state'],
		publishEntityState: Extension['publishEntityState'],
		eventBus: Extension['eventBus'],
		enableDisableExtension: Extension['enableDisableExtension'],
		restartCallback: Extension['restartCallback'],
		addExtension: Extension['addExtension'],
		settings: Settings,
		logger: Logger,
	) {
		super(zigbee, mqtt, state, publishEntityState, eventBus, enableDisableExtension, restartCallback, addExtension);
		this.REQUEST_REGEX = new RegExp(`${settings.get().mqtt.base_topic}/bridge/request/(.*)`);
		this.settings = settings;
		this.logger = logger;
	}

	override async start(): Promise<void> {
		this.eventBus.onMQTTMessage(this, this.onMQTTMessage);
	}

	@bind
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
}

module.exports = OperatorExtension;
