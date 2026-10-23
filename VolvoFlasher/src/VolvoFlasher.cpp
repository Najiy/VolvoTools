#include <common/compression/BoschCompressor.hpp>
#include <common/compression/CompressorFactory.hpp>
#include <common/encryption/EncryptorFactory.hpp>
#include <common/encryption/XOREncryptor.hpp>
#include <common/protocols/D2ECUType.hpp>
#include <common/protocols/D2Messages.hpp>
#include <common/protocols/TP20RequestProcessor.hpp>
#include <common/protocols/TP20Session.hpp>
#include <common/protocols/UDSProtocolCommonSteps.hpp>
#include <common/protocols/UDSPinFinder.hpp>
#include <common/protocols/UDSRequest.hpp>
#include <common/CanAlarmClock.hpp>
#include <common/ChecksumHelper.hpp>
#include <common/CommonData.hpp>
#include <common/ProtocolType.hpp>
#include <common/VBFParser.hpp>
#include <common/VBFUtil.hpp>
#include <common/SBL.hpp>
#include <common/Util.hpp>
#include <common/utility.hpp>

#include <j2534/J2534.hpp>
#include <j2534/J2534Channel.hpp>

#include <flasher/D2Flasher.hpp>
#include <flasher/ReaderBase.hpp>
#include <flasher/ReaderFactory.hpp>
#include <flasher/ReaderParametersProviderBase.hpp>
#include <flasher/SBLProviderVBF.hpp>
#include <flasher/SBLProviderCommon.hpp>
#include <flasher/UDSFlasher.hpp>
#include <flasher/KWPFlasher.hpp>

#include <argparse/argparse.hpp>

#define LOG_MODULE_NAME "flasher"
#include <common/LogHelper.hpp>

#include <algorithm>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <fstream>
#include <iostream>
#include <iterator>
#include <mutex>
#include <sstream>
#include <thread>
#include <chrono>
#include <iomanip>
#include <unordered_map>

INITIALIZE_EASYLOGGINGPP

bool stopRequested = false;

enum class RunMode
{
	None,
	Flash,
	Read,
	Wakeup,
	Pin,
	Test,
	Devices
};

unsigned long hexStringToUlong(const std::string& str)
{
    std::stringstream ss;
    ss << str;
    unsigned long result = 0;
    ss >> std::hex >> result;
    return result;
}

const char* runModeName(RunMode mode)
{
    switch (mode) {
    case RunMode::Flash: return "flash";
    case RunMode::Read: return "read";
    case RunMode::Wakeup: return "wakeup";
    case RunMode::Pin: return "pin";
    case RunMode::Test: return "test";
    case RunMode::Devices: return "devices";
    default: return "none";
    }
}

// ---------------------------------------------------------------------------------------------
// Machine-readable mode for the VolvoTools UI (--json).
//
// Structured output is one JSON object per line on stdout, each with a "type" member:
// hello, devices, state, progress, plan and result (always last). The format is described in
// docs/tech_specs/ui_json_protocol.md. Anything else that gets printed, such as library
// logging, is plain text and is shown to the user as a log line.
// ---------------------------------------------------------------------------------------------

constexpr int kJsonProtocolVersion = 1;

bool g_json = false;
bool g_resultEmitted = false;
int g_exitCode = 0;
std::mutex g_jsonMutex;

struct ExtraOptions {
    std::string platform;  // -f as typed, echoed in the plan
    std::string cmInfo;    // --cm-info: variant of the ECU software, for example me9_p1
    bool dryRun = false;
    std::string emitImage;
};

std::string jsonEscape(const std::string& text)
{
    std::string out;
    out.reserve(text.size());
    for (const unsigned char c : text) {
        switch (c) {
        case '"':
            out += "\\\"";
            break;
        case '\\':
            out += "\\\\";
            break;
        case '\n':
            out += "\\n";
            break;
        case '\r':
            out += "\\r";
            break;
        case '\t':
            out += "\\t";
            break;
        default:
            if (c < 0x20) {
                char buffer[8];
                std::snprintf(buffer, sizeof(buffer), "\\u%04x", static_cast<unsigned>(c));
                out += buffer;
            }
            else {
                out += static_cast<char>(c);
            }
        }
    }
    return out;
}

std::string jsonString(const std::string& text)
{
    return "\"" + jsonEscape(text) + "\"";
}

const char* jsonBool(bool value)
{
    return value ? "true" : "false";
}

void emitJson(const std::string& line)
{
    std::lock_guard<std::mutex> lock(g_jsonMutex);
    std::cout << line << std::endl;
}

void emitHello(const char* command)
{
    emitJson("{\"type\":\"hello\",\"protocol\":" + std::to_string(kJsonProtocolVersion) +
        ",\"app\":\"VolvoFlasher\",\"version\":\"1.0\",\"command\":" + jsonString(command) + "}");
}

void emitResult(bool ok, const std::string& message)
{
    {
        std::lock_guard<std::mutex> lock(g_jsonMutex);
        g_resultEmitted = true;
    }
    emitJson(std::string("{\"type\":\"result\",\"ok\":") + jsonBool(ok) + ",\"message\":" + jsonString(message) + "}");
}

const char* stateName(flasher::FlasherState state)
{
    using flasher::FlasherState;
    switch (state) {
    case FlasherState::Initial: return "Initial";
    case FlasherState::OpenChannels: return "OpenChannels";
    case FlasherState::FallAsleep: return "FallAsleep";
    case FlasherState::Authorize: return "Authorize";
    case FlasherState::ProgrammingSession: return "ProgrammingSession";
    case FlasherState::LoadBootloader: return "LoadBootloader";
    case FlasherState::StartBootloader: return "StartBootloader";
    case FlasherState::RequestDownload: return "RequestDownload";
    case FlasherState::EraseFlash: return "EraseFlash";
    case FlasherState::WriteFlash: return "WriteFlash";
    case FlasherState::CheckFlash: return "CheckFlash";
    case FlasherState::ReadFlash: return "ReadFlash";
    case FlasherState::WakeUp: return "WakeUp";
    case FlasherState::CloseChannels: return "CloseChannels";
    case FlasherState::Done: return "Done";
    case FlasherState::Error: return "Error";
    }
    return "Unknown";
}

void emitState(flasher::FlasherState state)
{
    emitJson(std::string("{\"type\":\"state\",\"state\":\"") + stateName(state) + "\"}");
}

void emitProgress(size_t current, size_t maximum)
{
    emitJson("{\"type\":\"progress\",\"current\":" + std::to_string(current) +
        ",\"max\":" + std::to_string(maximum) + "}");
}

void emitDevices(const std::vector<j2534::DeviceInfo>& devices)
{
    std::string line = "{\"type\":\"devices\",\"devices\":[";
    for (size_t i = 0; i < devices.size(); ++i) {
        if (i) {
            line += ",";
        }
        line += "{\"name\":" + jsonString(devices[i].deviceName) +
            ",\"library\":" + jsonString(devices[i].libraryName) + "}";
    }
    line += "]}";
    emitJson(line);
}

// Last line of a hardware job: the legacy text, or the JSON result in --json mode.
void reportResult(bool success, const std::string& okText, const std::string& failText)
{
    if (g_json) {
        emitResult(success, success ? okText : failText);
        if (!success) {
            g_exitCode = 1;
        }
        return;
    }
    std::cout << std::endl << (success ? okText : failText) << std::endl;
}

void reportException(const std::string& what)
{
    if (g_json) {
        emitResult(false, what);
        g_exitCode = 1;
    }
}

// Waits until a flasher or reader is done. Prints dots, or progress lines in --json mode.
template<typename Worker>
bool waitForCompletion(Worker& worker)
{
    size_t lastCurrent = static_cast<size_t>(-1);
    size_t lastMaximum = static_cast<size_t>(-1);
    const auto reportProgress = [&]() {
        const size_t current = worker.getCurrentProgress();
        const size_t maximum = worker.getMaximumProgress();
        if (current != lastCurrent || maximum != lastMaximum) {
            lastCurrent = current;
            lastMaximum = maximum;
            emitProgress(current, maximum);
        }
    };
    while (worker.getCurrentState() != flasher::FlasherState::Done &&
           worker.getCurrentState() != flasher::FlasherState::Error) {
        if (g_json) {
            std::this_thread::sleep_for(std::chrono::milliseconds(250));
            reportProgress();
        }
        else {
            std::this_thread::sleep_for(std::chrono::seconds(1));
            std::cout << ".";
        }
    }
    if (g_json) {
        reportProgress();
    }
    return worker.getCurrentState() == flasher::FlasherState::Done;
}

bool getRunOptions(int argc, const char* argv[], std::string& deviceName,
	unsigned long& baudrate, std::string& flashPath, uint64_t& pin,
	uint8_t& ecuId, unsigned long& start, unsigned long& datasize,
	RunMode& runMode, std::string& sblPath, common::CarPlatform& carPlatform, bool& pinUpward, bool& verbose,
	ExtraOptions& extra) {
	argparse::ArgumentParser program("VolvoFlasher", "1.0", argparse::default_arguments::help);
	program.add_argument("-d", "--device").default_value(std::string{}).help("Device name");
	program.add_argument("-b", "--baudrate").scan<'u', unsigned long>().default_value(500000ul).help("CAN bus speed");
	program.add_argument("-f", "--platform").default_value(std::string{ "P2" }).help("Car's platform, supported values: P80, P1, P1_UDS, P2, P2_250, P2_UDS, P3, SPA");
    program.add_argument("-e", "--ecu").default_value("7A").help("ECU id");
    program.add_argument("-p", "--pin").default_value("0x0").help("PIN to unlock ECU");
	program.add_argument("-v", "--verbose").default_value(false).implicit_value(true).nargs(0).help("Enable verbose (debug) logging");
	program.add_argument("--cm-info").default_value(std::string{}).help("Variant of the ECU software, selects the bootloader and flash layout, for example me9_p1");
	program.add_argument("--json").default_value(false).implicit_value(true).nargs(0).help("Machine-readable output for the VolvoTools UI");

	argparse::ArgumentParser flash_command("flash", "1.0", argparse::default_arguments::help);
	flash_command.add_description("Flash BIN to ECU");
	flash_command.add_argument("-i", "--input").help("File to flash");
	flash_command.add_argument("-s", "--sbl").default_value(std::string()).help("File with SBL");
	flash_command.add_argument("--dry-run").default_value(false).implicit_value(true).nargs(0).help("With --json: report what would be written and exit without touching the ECU");
	flash_command.add_argument("--emit-image").default_value(std::string()).help("With --dry-run: write the image that would be sent to this file");

	argparse::ArgumentParser read_command("read", "1.0", argparse::default_arguments::help);
	read_command.add_description("Read BIN from ECU");
	read_command.add_argument("-o", "--output").help("File to write");
    read_command.add_argument("-s", "--start").help("Begin address to read");
    read_command.add_argument("-sz", "--size").help("Datasize to read");

	argparse::ArgumentParser test_command("test", "1.0", argparse::default_arguments::help);
	test_command.add_description("Test purposes");

	argparse::ArgumentParser pin_command("pin", "1.0", argparse::default_arguments::help);
	pin_command.add_description("Bruteforce ECM PIN code. If you provide -p argument then program starts from providen PIN");
	pin_command.add_argument("-d", "--down").default_value(false).implicit_value(true).nargs(0).help("Scan pins downward");

	argparse::ArgumentParser wakeup_command("wakeup", "1.0", argparse::default_arguments::help);
	wakeup_command.add_description("Wake up CAN network");
	argparse::ArgumentParser devices_command("devices", "1.0", argparse::default_arguments::help);
	devices_command.add_description("List installed J2534 devices");

	program.add_subparser(flash_command);
	program.add_subparser(read_command);
	program.add_subparser(test_command);
	program.add_subparser(pin_command);
	program.add_subparser(wakeup_command);
	program.add_subparser(devices_command);
	try {
		program.parse_args(argc, argv);
		if (program.is_subcommand_used(flash_command)) {
			flashPath = flash_command.get("-i");
			sblPath = flash_command.get("-s");
			extra.dryRun = flash_command.get<bool>("--dry-run");
			extra.emitImage = flash_command.get<std::string>("--emit-image");
			runMode = RunMode::Flash;
		}
		else if (program.is_subcommand_used(read_command)) {
            flashPath = read_command.get("-o");
            start = hexStringToUlong(read_command.get("-s"));
            datasize = hexStringToUlong(read_command.get("-sz"));
			runMode = RunMode::Read;
		}
		else if (program.is_subcommand_used(test_command)) {
			runMode = RunMode::Test;
		}
		else if (program.is_subcommand_used(pin_command)) {
			pinUpward = !pin_command.get<bool>("-d");
			runMode = RunMode::Pin;
		}
		else if (program.is_subcommand_used(wakeup_command)) {
			runMode = RunMode::Wakeup;
		}
		else if (program.is_subcommand_used(devices_command)) {
			runMode = RunMode::Devices;
		}
		else {
			std::cout << program;
			return false;
		}
		deviceName = program.get("-d");
        baudrate = program.get<unsigned long>("-b");
        ecuId = static_cast<uint8_t>(hexStringToUlong(program.get("-e")));
        carPlatform = common::parseCarPlatform(program.get<std::string>("-f"));
		extra.platform = program.get<std::string>("-f");
		std::transform(extra.platform.begin(), extra.platform.end(), extra.platform.begin(),
			[](unsigned char c) { return static_cast<char>(std::toupper(c)); });
        pin = hexStringToUlong(program.get("-p"));
		verbose = program.get<bool>("-v");
		extra.cmInfo = program.get<std::string>("--cm-info");
		return true;
	}
	catch (const std::exception& err) {
		std::cerr << err.what() << std::endl;
		std::cerr << program;
	}
	return false;
#if 0
	runMode = RunMode::None;
	using namespace boost::program_options;
	options_description descr;
	descr.add_options()("device,d", value<std::string>()->default_value(""),
		"Device name")(
			"baudrate,b", value<unsigned long>()->default_value(500000),
			"CAN bus speed")("flash,f", value<std::string>(),
				"Path to flash BIN")
		("read,r", value<std::string>(),
					"Path to flash BIN")
		("test",
			"Test purposes")
		("start,s", value<unsigned long>()->default_value(0),
			"Begin address to read")
		("size,sz", value<unsigned long>()->default_value(0),
			"Datasize to read")
		("ecu,e", value<int>()->default_value(0x7A),
			"ECU id to read")
		("wakeup,w", "Wake up CAN network")("pin,p", value<unsigned long>(), "Bruteforce ECM PIN code, pass XXYYZZ start code or it started from 000000 code");
	command_line_parser parser{ argc, argv };
	parser.options(descr);
	variables_map vm;
	store(parser.run(), vm);
	if (vm.count("wakeup")) {
		deviceName = vm["device"].as<std::string>();
		baudrate = vm["baudrate"].as<unsigned long>();
		runMode = RunMode::Wakeup;
		return true;
	}
	else if (vm.count("flash")) {
		deviceName = vm["device"].as<std::string>();
		baudrate = vm["baudrate"].as<unsigned long>();
		flashPath = vm["flash"].as<std::string>();
		runMode = RunMode::Flash;
		return true;
	}
	else if (vm.count("pin")) {
		deviceName = vm["device"].as<std::string>();
		baudrate = vm["baudrate"].as<unsigned long>();
		pinStart = vm["pin"].as<unsigned long>();
		runMode = RunMode::Pin;
		return true;
	}
	else if (vm.count("read")) {
		deviceName = vm["device"].as<std::string>();
		baudrate = vm["baudrate"].as<unsigned long>();
		flashPath = vm["read"].as<std::string>();
		cmId = static_cast<uint8_t>(vm["ecu"].as<int>());
		start = vm["start"].as<unsigned long>();
		datasize = vm["size"].as<unsigned long>();
		runMode = RunMode::Read;
		return true;
	}
	else if (vm.count("test")) {
		deviceName = vm["device"].as<std::string>();
		baudrate = vm["baudrate"].as<unsigned long>();
		runMode = RunMode::Test;
	}
	else {
		std::cout << descr;
		return false;
	}
	return false;
#endif
}

class FlasherCallback final : public flasher::FlasherCallback {
public:
    FlasherCallback() = default;

	void OnProgress(std::chrono::milliseconds timePoint, size_t currentValue,
		size_t maxValue) override {
	}

    void OnState(flasher::FlasherState state) override {
        if (g_json) {
            emitState(state);
            return;
        }
        std::cout << std::endl;
        using flasher::FlasherState;
        switch(state) {
        case FlasherState::Initial:
            std::cout << "Starting";
            break;
        case FlasherState::OpenChannels:
            std::cout << "Open channels";
            break;
        case FlasherState::FallAsleep:
            std::cout << "Go to sleep";
            break;
        case FlasherState::Authorize:
            std::cout << "Authorizing";
            break;
		case FlasherState::ProgrammingSession:
			std::cout << "Enter programming session";
			break;
		case FlasherState::LoadBootloader:
            std::cout << "Bootloader loading";
            break;
        case FlasherState::StartBootloader:
            std::cout << "Bootloader starting";
            break;
		case FlasherState::RequestDownload:
			std::cout << "Request download";
			break;
		case FlasherState::CheckFlash:
            std::cout << "Checking flash";
            break;
		case FlasherState::EraseFlash:
            std::cout << "Flash erasing";
            break;
        case FlasherState::WriteFlash:
            std::cout << "Flash writing";
            break;
        case FlasherState::ReadFlash:
            std::cout << "Flash reading";
            break;
        case FlasherState::WakeUp:
            std::cout << "Waking up";
            break;
        case FlasherState::CloseChannels:
            std::cout << "Close channels";
            break;
        case FlasherState::Done:
            std::cout << "Done";
            break;
        case FlasherState::Error:
            std::cout << "Error";
            break;
        }
    }
};

uint32_t ford_seed(uint32_t seed, uint8_t* key)
{
	uint8_t sa[8];
	uint32_t  f = 0xc541a9;
	uint32_t  f1 = 0;
	uint32_t  i;

	sa[0x0] = seed >> 16;
	sa[0x1] = seed >> 8;
	sa[0x2] = seed;

	sa[0x3] = key[0];
	sa[0x4] = key[1];
	sa[0x5] = key[2];
	sa[0x6] = key[3];
	sa[0x7] = key[4];

	for (i = 0; i < 0x40; i++)
	{
		f1 = f >> 1;

		if (((1 << (i & 7)) & sa[i >> 3]) == 0) { if ((f & 1) == 1) f1 |= 0x800000; }
		else { if ((f & 1) == 0) f1 |= 0x800000; }

		if ((f1 & 0x800000) == 0x800000) { f = f1 ^ 0x109028; }
		else { f = f1; }
	}

	f1 = ((f >> 4) & 0xf) | ((f & 0xf00) >> 4); f1 = f1 << 8;
	f1 += ((f >> 20) | ((f & 0xf000) >> 8)); f1 = f1 << 8;
	f1 += (((f & 0xf) << 4) | ((f >> 16) & 0xf));

	return(f1);
}

uint32_t SK1_FordME9(uint32_t seed, uint8_t output_key[4])
{
	uint32_t key = 0;
	uint64_t Key64 = 0xA17E86;
	uint8_t secretKey[5];

	// Generate the 5 Byte Array
	secretKey[0] = Key64 & 0xFF;
	secretKey[1] = (Key64 >> 8) & 0xFF;
	secretKey[2] = (Key64 >> 16) & 0xFF;
	secretKey[3] = (Key64 >> 24) & 0xFF;
	secretKey[4] = (Key64 >> 32) & 0xFF;

	key = ford_seed(seed, secretKey);
	output_key[0] = key & 0xFF;
	output_key[1] = (key >> 8) & 0xFF;
	output_key[2] = (key >> 16) & 0xFF;
	output_key[3] = (key >> 24) & 0xFF;
	return key;
}

uint32_t VolvoGenerateKey(const std::array<uint8_t, 5> pin_array, uint8_t seed_array[3], uint8_t key_array[3])
{
	unsigned int high_part;
	unsigned int low_part;
	unsigned int hash = 0xC541A9;
	uint8_t old;
	uint8_t is_bit_set;
	uint32_t result;

	high_part = pin_array[4] << 24 | pin_array[3] << 16 | pin_array[2] << 8 | pin_array[1];
	low_part = pin_array[0] << 24 | seed_array[2] << 16 | seed_array[1] << 8 | seed_array[0];
	for (size_t i = 0; i < 32; ++i)
	{
		old = low_part;
		low_part >>= 1;
		is_bit_set = hash ^ old;
		hash >>= 1;
		if ((is_bit_set & 1) != 0)
			hash = (hash | 0x800000) ^ 0x109028;
	}
	for (size_t i = 0; i < 32; ++i)
	{
		old = high_part;
		high_part >>= 1;
		is_bit_set = hash ^ old;
		hash >>= 1;
		if ((is_bit_set & 1) != 0)
			hash = (hash | 0x800000) ^ 0x109028;
	}
	result = ((hash & 0xF00000) >> 12) | hash & 0xF000 | (uint8_t)(16 * hash) | ((hash & 0xFF0) << 12) | ((hash & 0xF0000) >> 16);
	key_array[2] = result & 0xFF;
	key_array[0] = (result >> 16) & 0xFF;
	key_array[1] = (result >> 8) & 0xFF;
	return result;
}

uint32_t p3_hash(const uint8_t pin[5], const uint8_t seed[3])
{
	uint32_t n = 0xc541a9, m = 0x1212050;
	uint64_t k;
	uint8_t* in = (unsigned char*)&k;

	in[0] = seed[0];
	in[1] = seed[1];
	in[2] = seed[2];
	in[3] = pin[0];
	in[4] = pin[1];
	in[5] = pin[2];
	in[6] = pin[3];
	in[7] = pin[4];

	for (size_t i = 0; i < 64; i++, n >>= 1, k >>= 1) {
		if ((n ^ k) & 0x1)
			n ^= m;
	}
	return ((n & 0xF00000) >> 12) | n & 0xF000 | (uint8_t)(16 * n) | ((n & 0xFF0) << 12) | ((n & 0xF0000) >> 16);
}

J2534_ERROR_CODE sendMessage(j2534::J2534Channel& channel, unsigned long protocolId, const std::vector<uint8_t>& data)
{
	PASSTHRU_MSG msg;
	memset(&msg, 0, sizeof(msg));
	for (size_t i = 0; i < data.size(); ++i) {
		msg.Data[i + 2] = data[i];
	}
	msg.DataSize = std::max(data.size() + 2, static_cast<size_t>(12));
	msg.ProtocolID = protocolId;
	unsigned long numMsgs = 1;
	return channel.writeMsgs({ msg }, numMsgs);
}

std::vector<PASSTHRU_MSG> readMessages(j2534::J2534Channel& channel, size_t messageNum = 1)
{
	std::vector<PASSTHRU_MSG> read_msgs;
	read_msgs.resize(messageNum);
	if (channel.readMsgs(read_msgs, 5000) != STATUS_NOERROR || read_msgs.empty())
	{
		std::cout << "Can't read message" << std::endl;
		LOG_MODULE(ERROR) << "Can't read message";
		return {};
	}
	return read_msgs;
}

bool authByKey(j2534::J2534Channel& channel, unsigned long protocolId, const std::array<uint8_t, 5>& pin)
{
	bool success = false;
	for (int l = 0; l < 10; ++l)
	{

		if (sendMessage(channel, protocolId, { 0x07, 0xE0, 0x02, 0x27, 0x01 }) == STATUS_NOERROR)
		{
			success = true;
			break;
		}
		else
			std::cout << "Retry send request seed message" << std::endl;
	}
	if (!success)
	{
		std::cout << "Can't request seed" << std::endl;
		LOG_MODULE(ERROR) << "Can't request seed";
		return false;
	}
	std::vector<PASSTHRU_MSG> read_msgs;
	read_msgs.resize(1);
	if (channel.readMsgs(read_msgs, 5000) != STATUS_NOERROR || read_msgs.empty())
	{
		std::cout << "Can't read seed" << std::endl;
		LOG_MODULE(ERROR) << "Can't read seed";
		return false;
	}
	uint8_t seed[3] = { read_msgs[0].Data[7], read_msgs[0].Data[8], read_msgs[0].Data[9] };
	uint8_t key[4];
	VolvoGenerateKey(pin, seed, key);
	channel.clearRx();
	if (sendMessage(channel, protocolId, { 0x07, 0xE0, 5, 0x27, 0x02, key[0], key[1], key[2] }) != STATUS_NOERROR)
	{
		std::cout << "Can't write key" << std::endl;
		LOG_MODULE(ERROR) << "Can't write key";
		return false;
	}
	read_msgs.resize(1);
	if (channel.readMsgs(read_msgs) != STATUS_NOERROR || read_msgs.empty())
	{
		std::cout << "Can't read key result" << std::endl;
		LOG_MODULE(ERROR) << "Can't read key result";
		return false;
	}
	const auto& answer_data = read_msgs[0].Data;
	if (answer_data[4] == 2 && answer_data[5] == 0x67 && answer_data[6] == 2)
	{
		return true;
	}
	return false;
}

void findPin(j2534::J2534& j2534, uint64_t i = 0)
{
	const unsigned long baudrate = 500000;
	j2534::J2534Channel channel(j2534, CAN, CAN_ID_BOTH, baudrate, 0);
	PASSTHRU_MSG msg;
	memset(&msg, 0, sizeof(msg));
	msg.ProtocolID = CAN;
	msg.DataSize = 4;
	msg.Data[2] = 7;
	unsigned long msg_id;
	channel.startMsgFilter(PASS_FILTER, &msg, &msg, nullptr, msg_id);
	memset(&msg, 0, sizeof(msg));
	msg.ProtocolID = CAN;
	msg.DataSize = 12;
	msg.Data[2] = 0x7;
	msg.Data[3] = 0xDF;
	msg.Data[4] = 0x02;
	msg.Data[5] = 0x10;
	msg.Data[6] = 0x82;

	std::cout << "Start sending prog" << std::endl;
	if (channel.startPeriodicMsg(msg, msg_id, 5) != STATUS_NOERROR)
	{
		std::cout << "Can't start prog periodic message" << std::endl;
		LOG_MODULE(ERROR) << "Can't start prog periodic message";
		return;
	}
	std::this_thread::sleep_for(std::chrono::seconds(4));
	std::cout << "Stop sending prog" << std::endl;
	channel.stopPeriodicMsg(msg_id);

	memset(&msg, 0, sizeof(msg));
	msg.ProtocolID = CAN;
	msg.DataSize = 12;
	msg.Data[2] = 0x7;
	msg.Data[3] = 0xDF;
	msg.Data[4] = 0x02;
	msg.Data[5] = 0x3E;
	msg.Data[6] = 0x80;
	std::cout << "Start sending alive messages" << std::endl;
	if (channel.startPeriodicMsg(msg, msg_id, 1900) != STATUS_NOERROR)
	{
		std::cout << "Can't start keep alive periodic message" << std::endl;
		LOG_MODULE(ERROR) << "Can't start keep alive periodic message";
		return;
	}
	for (; i <= 0xFFFFFF; ++i)
	{
		const uint8_t p1 = (i >> 16) & 0xFF;
		const uint8_t p2 = (i >> 8) & 0xFF;
		const uint8_t p3 = i & 0xFF;
		if (!p3)
			std::cout << "Trying PIN " << std::hex << std::setfill('0') << std::setw(2) << int(p1) << " "
			<< std::hex << std::setfill('0') << std::setw(2) << int(p2) << " XX" << std::endl;

		std::array<uint8_t, 5> pin = { 0, 0, p1, p2, p3 };
		if(authByKey(channel, CAN, pin))
		{
			std::cout << "Found PIN code 00 00 "
				<< std::hex << std::setfill('0') << std::setw(2) << int(p1) << " "
				<< std::hex << std::setfill('0') << std::setw(2) << int(p2) << " "
				<< std::hex << std::setfill('0') << std::setw(2) << int(p3) << std::endl;
			return;
		}
	}
}

void findPin2(j2534::J2534& j2534, common::CarPlatform carPlatform, uint8_t ecuId, uint64_t startPin = 0, bool upward = true)
{
	std::chrono::time_point savedTime = std::chrono::steady_clock::now();
	uint64_t savedPin = startPin;
	common::UDSPinFinder pinFinder(j2534, carPlatform, ecuId, [&savedTime, &savedPin](common::UDSPinFinder::State state, uint64_t currentPin) {
		switch (state) {
		case common::UDSPinFinder::State::FallAsleep:
			std::cout << "Fall asleep" << std::endl;
			break;
		case common::UDSPinFinder::State::KeepAlive:
			std::cout << "Start keep alive" << std::endl;
			break;
		case common::UDSPinFinder::State::Work:
			if (!(currentPin & 0xFF)) {
				const auto now = std::chrono::steady_clock::now();
				uint64_t pinDiff = currentPin > savedPin ? currentPin - savedPin : savedPin - currentPin;
				const auto pinPerSec = pinDiff / ((now - savedTime).count() / 1000000000);
				savedPin = currentPin;
				savedTime = now;
				std::cout << "Trying PIN " << std::hex << currentPin << ", " << std::dec << pinPerSec << " pins/sec" << std::endl;
			}
			break;
		}
		}, upward ? common::UDSPinFinder::Direction::Up : common::UDSPinFinder::Direction::Down, startPin);

	if (!pinFinder.start()) {
		std::cout << "Failed to start PIN finder" << std::endl;
	}
	else {
		while (pinFinder.getCurrentState() != common::UDSPinFinder::State::Done && pinFinder.getCurrentState() != common::UDSPinFinder::State::Error) {
			if (stopRequested) {
				pinFinder.stop();
				stopRequested = false;
			}
			std::this_thread::sleep_for(std::chrono::milliseconds(100));
		}
		if (const auto foundPin{ pinFinder.getFoundPin() }) {
			std::cout << "Found PIN code "
				<< std::hex << std::setfill('0') << *foundPin << std::endl;
		}
		else {
			std::cout << "Last checked PIN code "
				<< std::hex << std::setfill('0') << savedPin << std::endl;
		}
	}
}

bool switchToDiagSession(j2534::J2534Channel& channel, unsigned long protocolId, const std::array<uint8_t, 5>& pin)
{
	PASSTHRU_MSG msg;

#if 1
	memset(&msg, 0, sizeof(msg));
	msg.ProtocolID = protocolId;
	msg.DataSize = 12;
	msg.Data[2] = 0x7;
	msg.Data[3] = 0xE0;// 0xDF;
	msg.Data[4] = 0x02;
	msg.Data[5] = 0x10;
	msg.Data[6] = 0x02;
	std::cout << "Start sending prog" << std::endl;
	unsigned long msg_id;
	if (channel.startPeriodicMsg(msg, msg_id, 5) != STATUS_NOERROR)
	{
		std::cout << "Can't start prog periodic message" << std::endl;
		LOG_MODULE(ERROR) << "Can't start prog periodic message";
		return false;
	}
	std::this_thread::sleep_for(std::chrono::seconds(4));
	std::cout << "Stop sending prog" << std::endl;
	channel.stopPeriodicMsg(msg_id);
#endif

	memset(&msg, 0, sizeof(msg));
	msg.ProtocolID = protocolId;
	msg.DataSize = 12;
	msg.Data[2] = 0x7;
	msg.Data[3] = 0xDF;
	msg.Data[4] = 0x02;
	msg.Data[5] = 0x3E;
	msg.Data[6] = 0x80;
	std::cout << "Start sending alive messages" << std::endl;
	if (channel.startPeriodicMsg(msg, msg_id, 1900) != STATUS_NOERROR)
	{
		std::cout << "Can't start keep alive periodic message" << std::endl;
		LOG_MODULE(ERROR) << "Can't start keep alive periodic message";
		return false;
	}

	for (int i = 0; i < 10; ++i) {
		if (authByKey(channel, CAN, pin))
		{
			std::cout << "Auth with PIN code 00 00 "
				<< std::hex << std::setfill('0') << std::setw(2) << int(pin[2]) << " "
				<< std::hex << std::setfill('0') << std::setw(2) << int(pin[3]) << " "
				<< std::hex << std::setfill('0') << std::setw(2) << int(pin[4]) << " success" << std::endl;
			return true;
		}
		std::this_thread::sleep_for(std::chrono::milliseconds(100));
	}
	return false;
}

template<int N, typename T>
std::array<uint8_t, N> toArray(T val)
{
    if(N > 0)
        return { static_cast<uint8_t>(val >> ((N + 1) * 8)), toArray<N - 1>(val)};
    return {};
}

void requestAndPrint(common::TP20Session& session, const std::vector<uint8_t>& request)
{
	const auto requestResult{ session.process(request) };
	if (!requestResult.empty()) {
		std::cout << "Request result: " << std::string(requestResult.begin() + 4, requestResult.end()) << std::endl;
	}
}

void doSomeStuff(std::unique_ptr<j2534::J2534> j2534, uint64_t pin)
{
	const auto carPlatform{ common::CarPlatform::VAG_MED91 };
//	const auto channel{ common::openTP20Channel(*j2534, 500000, 0x201) };
    const auto ecuId{ 0x01 };
	const std::string binPath{ "C:\\misc\\gcflasher\\8P0907115K_0040_2_0l_R4_4V_TFSI_extFLASH_CHKFIXED.bin" };
	std::ifstream binStream(binPath, std::ios_base::binary);
	const auto flashVbf{ common::loadVBFForFlasher(carPlatform, ecuId, {}, binPath, binStream) };
	//common::TP20Session session{ *channel, carPlatform, ecuId };
	//if (!session.start()) {
	//	std::cout << "Failed to start TP20 session" << std::endl;
	//	return;
	//}
//    common::TP20RequestProcessor requestProcessor{session};
    std::array<uint8_t, 5> pinArray = {
        (pin >> 32) & 0xFF, (pin >> 24) & 0xFF, (pin >> 16) & 0xFF, (pin >> 8) & 0xFF, pin & 0xFF };
    flasher::KWPFlasherConfig config{
        {}, pinArray, flashVbf, common::CompressionType::Bosch };
    flasher::KWPFlasher flasher{ *j2534, carPlatform, ecuId, std::move(config) };
	FlasherCallback callback;
	flasher.registerCallback(callback);
	flasher.start();
	while (flasher.getCurrentState() !=
		flasher::FlasherState::Done && flasher.getCurrentState() !=
		flasher::FlasherState::Error) {
		std::this_thread::sleep_for(std::chrono::seconds(1));
		std::cout << ".";
	}
	const bool success = flasher.getCurrentState() ==
		flasher::FlasherState::Done;
	std::cout << std::endl
		<< ((success)
			? "Flashing done"
			: "Flashing error. Try again.")
		<< std::endl;
	//requestAndPrint(session, { 0x1a,  0x86 });
	//requestAndPrint(session, { 0x1a,  0x90 });
	//requestAndPrint(session, { 0x1a,  0x91 });
	//requestAndPrint(session, { 0x1a,  0x92 });
	//requestAndPrint(session, { 0x1a,  0x94 });
	//requestAndPrint(session, { 0x1a,  0x97 });
	//requestAndPrint(session, { 0x1a,  0x9A });
	//requestAndPrint(session, { 0x1a,  0x9B });
	//requestAndPrint(session, { 0x1a,  0x9C });
}

void UDSFlash(common::CarPlatform carPlatform, uint8_t ecuId,
	std::unique_ptr<j2534::J2534> j2534, unsigned long baudrate, uint64_t pin, const std::string& flashPath, const std::string& sblPath)
{
	common::VBFParser vbfParser;
	std::ifstream sblVbf(sblPath, std::ios_base::binary);
	const common::VBF bootloader{ vbfParser.parse(sblVbf) };
	std::ifstream flashVbf(flashPath, std::ios_base::binary);
	const common::VBF flash{ vbfParser.parse(flashVbf) };
    const auto ecuInfo{ common::getEcuInfoByEcuId(carPlatform, ecuId) };

    std::array<uint8_t, 5> pinArray = {
        (pin >> 32) & 0xFF, (pin >> 24) & 0xFF, (pin >> 16) & 0xFF, (pin >> 8) & 0xFF, pin & 0xFF };
    flasher::UDSFlasherConfig config{ pinArray, bootloader, flash };
    flasher::UDSFlasher flasher{ *j2534, carPlatform, ecuId, std::move(config) };
	FlasherCallback callback;
	flasher.registerCallback(callback);
	flasher.start();
	const bool success = waitForCompletion(flasher);
	reportResult(success, "Flashing done", "Flashing error. Try again.");
}

common::VBF vbfForFlasher(const std::vector<uint8_t>& input)
{
    return common::VBF({}, { { 0x0, input } });
}

void D2Flash(const std::string& flashPath, std::unique_ptr<j2534::J2534> j2534, unsigned long baudrate)
{
	std::fstream input(flashPath,
		std::ios_base::binary | std::ios_base::in);
	std::vector<uint8_t> bin{ std::istreambuf_iterator<char>(input), {} };

    const auto vbf = vbfForFlasher(bin);
	const auto carPlatform = baudrate == 500000 ? common::CarPlatform::P2 : common::CarPlatform::P2_250;

    const uint32_t ecuId = to_underlying(common::D2ECUType::ECM_ME);
	flasher::SBLProviderCommon sblProviderCommon;
    flasher::D2FlasherConfig config{ sblProviderCommon.getSBL(carPlatform, ecuId, ""), vbf };
    flasher::D2Flasher flasher(*j2534, carPlatform, ecuId, std::move(config));
	FlasherCallback callback;
	flasher.registerCallback(callback);
    flasher.start();
	const bool success = waitForCompletion(flasher);
	reportResult(success, "Flashing done", "Flashing error. Try again.");
}

std::string hexString(unsigned value)
{
    std::ostringstream stream;
    stream << std::uppercase << std::hex << value;
    return stream.str();
}

// Flashes a binary image on a D2 ECU with the bootloader and flash layout that belong to
// cmInfo, for example "me9_p1". The checksum correction and the list of areas to erase and
// write come from loadVBFForFlasher, so what runs here is what flashDryRun reports.
void D2FlashImage(common::CarPlatform carPlatform, uint8_t ecuId, const std::string& cmInfo,
    const std::string& flashPath, std::unique_ptr<j2534::J2534> j2534)
{
    std::ifstream input(flashPath, std::ios_base::binary);
    const common::VBF flash = common::loadVBFForFlasher(carPlatform, ecuId, cmInfo, flashPath, input);
    flasher::SBLProviderCommon sblProviderCommon;
    common::VBF bootloader = sblProviderCommon.getSBL(carPlatform, ecuId, cmInfo);
    if (flash.chunks.empty()) {
        reportResult(false, "", "The flash file is empty or could not be read");
        return;
    }
    // Without a bootloader the erase and write commands would go to the primary bootloader.
    if (bootloader.chunks.empty()) {
        reportResult(false, "", "There is no bootloader for this platform, ECU and --cm-info");
        return;
    }
    flasher::D2FlasherConfig config{ std::move(bootloader), flash };
    flasher::D2Flasher flasher(*j2534, carPlatform, ecuId, std::move(config));
    FlasherCallback callback;
    flasher.registerCallback(callback);
    flasher.start();
    const bool success = waitForCompletion(flasher);
    reportResult(success, "Flashing done", "Flashing error. Try again.");
}

// Reports what a flash of this file would do and touches no hardware: the bootloader, the areas
// to erase and write, how many bytes the checksum correction changed, and the checksum verdict
// on the file as given. With extra.emitImage it also writes the exact image that would be sent
// (0xFF outside the written areas) so a read-back can be compared against it.
bool flashDryRun(common::CarPlatform carPlatform, uint8_t ecuId, const ExtraOptions& extra,
    const std::string& flashPath)
{
    std::ifstream input(flashPath, std::ios_base::binary);
    if (!input) {
        emitResult(false, "Can't open " + flashPath);
        return false;
    }
    const std::vector<uint8_t> original{ std::istreambuf_iterator<char>(input), {} };

    std::vector<uint8_t> probe(original);
    util::ChecksumHelper checksum(carPlatform, ecuId, common::toLower(extra.cmInfo));
    const bool checksumSupported = checksum.isSupported(probe);
    const bool checksumValid = checksumSupported && checksum.check(probe);
    const size_t checkedBlocks = checksumSupported ? checksum.countCheckedBlocks(probe) : 0;

    std::ifstream again(flashPath, std::ios_base::binary);
    const common::VBF flash = common::loadVBFForFlasher(carPlatform, ecuId, extra.cmInfo, flashPath, again);
    flasher::SBLProviderCommon sblProviderCommon;
    const common::VBF bootloader = sblProviderCommon.getSBL(carPlatform, ecuId, extra.cmInfo);

    // For .vbf and .hex files the input is not a flat image, so there is nothing to compare with.
    const bool flatImage = flashPath.rfind(".vbf") == std::string::npos && flashPath.rfind(".hex") == std::string::npos;

    std::ostringstream plan;
    plan << "{\"type\":\"plan\",\"platform\":" << jsonString(extra.platform)
         << ",\"ecu\":" << jsonString(hexString(ecuId))
         << ",\"cmInfo\":" << jsonString(extra.cmInfo)
         << ",\"fileSize\":" << original.size()
         << ",\"checksum\":{\"supported\":" << jsonBool(checksumSupported)
         << ",\"valid\":" << jsonBool(checksumValid)
         << ",\"checkedBlocks\":" << checkedBlocks << "}"
         << ",\"bootloader\":{\"call\":" << bootloader.header.call << ",\"chunks\":[";
    for (size_t i = 0; i < bootloader.chunks.size(); ++i) {
        plan << (i ? "," : "") << "{\"start\":" << bootloader.chunks[i].writeOffset
             << ",\"length\":" << bootloader.chunks[i].data.size() << "}";
    }
    plan << "]},\"erase\":[";
    for (size_t i = 0; i < flash.header.eraseBlocks.size(); ++i) {
        plan << (i ? "," : "") << "{\"start\":" << flash.header.eraseBlocks[i].startAddr
             << ",\"length\":" << flash.header.eraseBlocks[i].length << "}";
    }
    plan << "],\"chunks\":[";
    for (size_t i = 0; i < flash.chunks.size(); ++i) {
        const auto& chunk = flash.chunks[i];
        size_t modified = 0;
        if (flatImage && static_cast<size_t>(chunk.writeOffset) + chunk.data.size() <= original.size()) {
            for (size_t b = 0; b < chunk.data.size(); ++b) {
                if (chunk.data[b] != original[chunk.writeOffset + b]) {
                    ++modified;
                }
            }
        }
        plan << (i ? "," : "") << "{\"start\":" << chunk.writeOffset << ",\"length\":" << chunk.data.size()
             << ",\"modified\":" << modified << "}";
    }
    plan << "]}";
    emitJson(plan.str());

    if (!extra.emitImage.empty()) {
        std::vector<uint8_t> image(original.size(), 0xFF);
        for (const auto& chunk : flash.chunks) {
            if (static_cast<size_t>(chunk.writeOffset) + chunk.data.size() <= image.size()) {
                std::copy(chunk.data.begin(), chunk.data.end(), image.begin() + chunk.writeOffset);
            }
        }
        std::ofstream out(extra.emitImage, std::ios_base::binary);
        out.write(reinterpret_cast<const char*>(image.data()), image.size());
        out.flush();
        if (!out) {
            emitResult(false, "Can't write " + extra.emitImage);
            return false;
        }
    }
    emitResult(true, "Dry run");
    return true;
}

uint16_t crc16(const uint8_t* data_p, size_t length) {
	unsigned char x;
	uint16_t crc = 0xFFFF;

	while (length--) {
		x = crc >> 8 ^ *data_p++;
		x ^= x >> 4;
		crc = (crc << 8) ^ ((unsigned short)(x << 12)) ^ ((unsigned short)(x << 5)) ^ ((unsigned short)x);
	}
	return crc;
}

std::unordered_map<uint16_t, uint8_t> g_crc16_map;

void fill_crc_map()
{
	for (size_t i = 0; i < 255; ++i)
	{
		const uint8_t data = i & 0xFF;
		const auto crc = crc16(&data, 1);
		g_crc16_map[crc] = data;
	}
}

void readFlash(std::unique_ptr<j2534::J2534> j2534, common::CarPlatform carPlatform, uint8_t ecuId, const std::string& cmInfo, const std::string& flashPath, unsigned long start, unsigned long datasize)
{
    class CLIReaderProvider final : public flasher::ReaderParametersProviderBase {
    public:
        CLIReaderProvider(common::CarPlatform platform, uint32_t id, const std::string& cmInfo,
                          uint32_t s, size_t sz)
            : ReaderParametersProviderBase(platform, id, cmInfo, std::make_unique<flasher::SBLProviderCommon>())
            , _range{ s, sz } {}
        flasher::ReadRanges getReadRanges() const override { return {_range}; }
    private:
        flasher::ReadRange _range;
    };

    auto provider = CLIReaderProvider(carPlatform, ecuId, cmInfo,
        static_cast<uint32_t>(start), static_cast<size_t>(datasize));
    auto reader = flasher::ReaderFactory::create(*j2534, provider);
    FlasherCallback callback;
    reader->registerCallback(callback);
    reader->start();
    bool success = waitForCompletion(*reader);
    std::string failure = "Reading error. Try again.";
    if (success) {
        const auto& bin = reader->buffers()[0];
        if (bin.size() != datasize) {
            success = false;
            failure = "The ECU returned " + std::to_string(bin.size()) + " bytes, expected " + std::to_string(datasize);
        }
        else {
            std::fstream output(flashPath, std::ios_base::binary | std::ios_base::out);
            output.write((const char*)bin.data(), bin.size());
            output.flush();
            if (!output) {
                success = false;
                failure = "Can't write " + flashPath;
            }
        }
    }
    reportResult(success, "Reading done", failure);
}

std::vector<uint32_t> getCorrectPins(const uint8_t seed[3], const uint8_t key[3])
{
	std::vector<uint32_t> result;
	uint8_t hash_array[4];
	uint8_t pin[5] = { 0, 0, 0, 0, 0 };
	for (uint32_t i = 0; i <= 0xFFFFFF; ++i)
	{
		pin[2] = (i >> 16) & 0xFF;
		pin[3] = (i >> 8) & 0xFF;
		pin[4] = i & 0xFF;
		const auto hash = p3_hash(pin, seed);
		hash_array[0] = hash & 0xFF;
		hash_array[1] = (hash >> 8) & 0xFF;
		hash_array[2] = (hash >> 16) & 0xFF;
		hash_array[3] = (hash >> 24) & 0xFF;

		if (hash_array[2] == key[0] && hash_array[1] == key[1] && hash_array[0] == key[2])
			result.push_back(i);
	}
	return result;
}

std::unordered_map<uint32_t, std::vector<uint32_t>> getKeysByPins(const uint8_t seed[3], const uint8_t key[3])
{
	std::unordered_map<uint32_t, std::vector<uint32_t>> result;
	uint8_t hash_array[4];
	uint8_t pin[5] = { 0, 0, 0, 0, 0 };
	for (uint32_t i = 0; i <= 0xFFFFFF; ++i)
	{
		pin[2] = (i >> 16) & 0xFF;
		pin[3] = (i >> 8) & 0xFF;
		pin[4] = i & 0xFF;
		const auto hash = p3_hash(pin, seed);
		hash_array[0] = hash & 0xFF;
		hash_array[1] = (hash >> 8) & 0xFF;
		hash_array[2] = (hash >> 16) & 0xFF;
		hash_array[3] = (hash >> 24) & 0xFF;

		result[hash].push_back(i);
	}
	return result;
}

void findMultiplePins()
{
	const uint8_t seed1[3] = { 0xE5, 0x76, 0x4B };
	const uint8_t key1[3] = { 0x76, 0x58, 0x2F };
	const auto pins1 = getKeysByPins(seed1, key1);
	for (const auto& it : pins1)
	{
		if (it.second.size() > 1)
		{
			std::cout << "has collision: " << it.first << std::endl;
		}
	}
}

BOOL WINAPI HandlerRoutine(_In_ DWORD dwCtrlType) {
	stopRequested = true;
	return TRUE;
}

int main(int argc, const char* argv[]) {
    g_json = std::any_of(argv + 1, argv + argc, [](const char* arg) { return std::strcmp(arg, "--json") == 0; });
    common::initLogger("application.log");
    if (!SetConsoleCtrlHandler(HandlerRoutine, TRUE)) {
		throw std::runtime_error("Can't set console control hander");
	}
#if 0
	fill_crc_map();
//	findMultiplePins();
	const uint8_t seed1[3] = { 0xE5, 0x76, 0x4B };
	const uint8_t key1[3] = { 0x76, 0x58, 0x2F };
//	const auto pins1 = getCorrectPins(seed1, key1);

	const uint8_t seed2[3] = { 0x0F, 0x8C, 0xB3 };
	const uint8_t key2[3] = { 0x41, 0xFC, 0x61 };
//	const auto pins2 = getCorrectPins(seed2, key2);

	uint8_t output_ford_key[4];
	uint32_t ford_key = SK1_FordME9(0x332211, output_ford_key);
	uint8_t pin_array[] = { 0, 0, 0xd3, 0x5d, 0x6f };
	uint8_t seed[] = { 0xe5, 0x1e, 0x8f };
	uint8_t key_array[4];
	uint32_t volvo_key = VolvoGenerateKey(pin_array, seed, key_array);
	uint8_t p3_key_array[4];
	uint32_t p3_key = p3_hash(pin_array, seed);
	p3_key_array[0] = p3_key & 0xFF;
	p3_key_array[1] = (p3_key >> 8) & 0xFF;
	p3_key_array[2] = (p3_key >> 16) & 0xFF;
	p3_key_array[3] = (p3_key >> 24) & 0xFF;
	#endif
	unsigned long baudrate = 0;
	std::string deviceName;
	std::string flashPath;
	std::string sblPath;
	common::CarPlatform carPlatform = common::CarPlatform::Undefined;
	uint64_t pin = 0;
	unsigned long start = 0;
	unsigned long datasize = 0;
	uint8_t ecuId = 0;
	RunMode runMode = RunMode::None;
	bool scanPinsUpward = true;
	bool verbose = false;
	ExtraOptions extra;
	const auto devices = common::getAvailableDevices();
	if (getRunOptions(argc, argv, deviceName, baudrate, flashPath, pin, ecuId, start, datasize, runMode, sblPath, carPlatform, scanPinsUpward, verbose, extra)) {
        if (verbose) {
            common::initLogger("application.log", true, true);
        }
		if (carPlatform == common::CarPlatform::P1 && ecuId == to_underlying(common::D2ECUType::ECM_ME) && extra.cmInfo.empty()) {
			// The engine ECU of P1 is a Bosch ME9. Without this the ME7 bootloader would be used.
			extra.cmInfo = "me9_p1";
		}
		if (g_json) {
			emitHello(runModeName(runMode));
		}
		if (runMode == RunMode::Devices) {
			if (g_json) {
				emitDevices(devices);
				emitResult(true, "");
			}
			else {
				for (const auto& device : devices) {
					std::cout << "    " << device.deviceName << std::endl;
				}
			}
			return 0;
		}
		if (extra.dryRun) {
			if (!g_json) {
				std::cerr << "--dry-run needs --json" << std::endl;
				return 1;
			}
			try {
				return flashDryRun(carPlatform, ecuId, extra, flashPath) ? 0 : 1;
			}
			catch (const std::exception& ex) {
				reportException(ex.what());
				return 1;
			}
		}
		// In --json mode the UI names exactly one adapter and only that one is used.
		const j2534::DeviceInfo* jsonDevice = nullptr;
		if (g_json) {
			for (const auto& device : devices) {
				if (device.deviceName == deviceName) {
					jsonDevice = &device;
					break;
				}
			}
			if (!jsonDevice && !deviceName.empty()) {
				for (const auto& device : devices) {
					if (device.deviceName.find(deviceName) != std::string::npos) {
						jsonDevice = &device;
						break;
					}
				}
			}
			if (!jsonDevice) {
				emitResult(false, deviceName.empty() ? "No J2534 device selected" : "No J2534 device matches " + deviceName);
				return 1;
			}
		}
		const auto selected = [&](const auto& device) {
			if (g_json) {
				return &device == jsonDevice;
			}
			return deviceName.empty() || device.deviceName.find(deviceName) != std::string::npos;
		};
		for (const auto& device : devices) {
			if (selected(device)) {
				try {
					std::string name =
						device.deviceName.find("DiCE-") != std::string::npos
						? device.deviceName
						: "";
					std::unique_ptr<j2534::J2534> j2534{
						std::make_unique<j2534::J2534>(device.libraryName) };
					j2534->PassThruOpen(name);
                    if (runMode == RunMode::Wakeup) {
                        common::CanAlarmClock alarmClock(*j2534);
                        alarmClock.start();
					}
					else if (runMode == RunMode::Pin) {
						findPin2(*j2534, carPlatform, ecuId, pin, scanPinsUpward);
					}
					else if (runMode == RunMode::Read) {
						readFlash(std::move(j2534), carPlatform, ecuId, extra.cmInfo, flashPath, start, datasize);
					}
					else if (runMode == RunMode::Flash) {
                        const auto ecuInfo{ common::getEcuInfoByEcuId(carPlatform, ecuId) };
						if (std::get<0>(ecuInfo).protocol == common::ProtocolType::ISO15765) {
                            UDSFlash(carPlatform, ecuId, std::move(j2534), baudrate, pin, flashPath, sblPath);
						}
						else {
							if (!extra.cmInfo.empty()) {
								D2FlashImage(carPlatform, ecuId, extra.cmInfo, flashPath, std::move(j2534));
							}
							else {
								D2Flash(flashPath, std::move(j2534), baudrate);
							}
						}
					}
					else if (runMode == RunMode::Test) {
						doSomeStuff(std::move(j2534), pin);
					}
				}
			catch (const std::exception& ex) {
				std::cout << ex.what() << std::endl;
				LOG_MODULE(ERROR) << "Exception: " << ex.what();
				reportException(ex.what());
				}
				catch (const char* ex) {
				std::cout << ex << std::endl;
				LOG_MODULE(ERROR) << "Exception: " << ex;
				reportException(ex);
				}
				catch (...) {
				std::cout << "exception" << std::endl;
				LOG_MODULE(ERROR) << "Unknown exception";
				reportException("Unknown exception");
				}
			}
		}
	}
	else {
		if (g_json) {
			emitResult(false, "Invalid command line, see the error output");
			return 1;
		}
		std::cout << "Available J2534 devices:" << std::endl;
		for (const auto& device : devices) {
			std::cout << "    " << device.deviceName << std::endl;
		}
	}
	if (g_json && !g_resultEmitted) {
		emitResult(true, "");
	}
	return g_json ? g_exitCode : 0;
}
