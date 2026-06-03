import XCTest
@testable import AlwaysApp

final class AlwaysAppTests: XCTestCase {
    
    override func setUpWithError() throws {
        // Put setup code here. This method is called before the invocation of each test method in the class.
    }
    
    override func tearDownWithError() throws {
        // Put teardown code here. This method is called after the invocation of each test method in the class.
    }
    
    func testExample() throws {
        // This is an example of a functional test case.
        // Use XCTAssert and related functions to verify your tests produce the correct results.
        XCTAssertTrue(true)
    }
    
    func testDaemonEventDecoding() throws {
        // Test that DaemonEvent can be decoded from JSON
        let json = """
        {
            "type": "ListeningStarted",
            "data": null
        }
        """
        
        let data = json.data(using: .utf8)!
        let decoder = JSONDecoder()
        
        do {
            let event = try decoder.decode(DaemonEvent.self, from: data)
            XCTAssertEqual(event.type, .listeningStarted)
        } catch {
            XCTFail("Failed to decode DaemonEvent: \(error)")
        }
    }
    
    func testDaemonEventEncoding() throws {
        // Test that DaemonEvent can be encoded to JSON
        // DaemonEvent only has a Codable initializer, so we decode first then encode
        let json = """
        {
            "type": "ListeningStarted",
            "data": null
        }
        """
        
        let data = json.data(using: .utf8)!
        let decoder = JSONDecoder()
        let encoder = JSONEncoder()
        
        do {
            let event = try decoder.decode(DaemonEvent.self, from: data)
            let encodedData = try encoder.encode(event)
            let encodedJson = String(data: encodedData, encoding: .utf8)!
            XCTAssertTrue(encodedJson.contains("ListeningStarted"))
        } catch {
            XCTFail("Failed to encode DaemonEvent: \(error)")
        }
    }
    
    func testSocketPathResolution() throws {
        // Test that the socket path is resolved correctly
        let path = UDSClient.defaultSocketPath()
        
        #if os(macOS)
        XCTAssertTrue(path.contains("Library/Caches/Always"), "Socket path should use Library/Caches/Always on macOS")
        #else
        XCTAssertTrue(path.contains("always.sock"), "Socket path should end with always.sock")
        #endif
    }
    
    func testConfigModel() throws {
        // Test Config model decoding with camelCase keys
        let json = """
        {
            "sttEnergyThreshold": 0.5,
            "hearEnergyThreshold": 0.3,
            "sttCooldownMs": 150,
            "sttSilence": 0.4,
            "sttAutoEnter": true,
            "groqApiKey": null,
            "sileroThreshold": 0.5
        }
        """
        
        let data = json.data(using: .utf8)!
        let decoder = JSONDecoder()
        
        do {
            let config = try decoder.decode(Config.self, from: data)
            XCTAssertEqual(config.sttEnergyThreshold, 0.5)
            XCTAssertEqual(config.hearEnergyThreshold, 0.3)
            XCTAssertEqual(config.sttCooldownMs, 150)
            XCTAssertEqual(config.sttSilence, 0.4)
            XCTAssertTrue(config.sttAutoEnter)
            XCTAssertEqual(config.sileroThreshold, 0.5)
        } catch {
            XCTFail("Failed to decode Config: \(error)")
        }
    }
    
    func testDaemonStatusModel() throws {
        // Test DaemonStatus model decoding with camelCase keys
        let json = """
        {
            "isRunning": true,
            "pid": 12345,
            "logPath": "/var/log/always.log"
        }
        """
        
        let data = json.data(using: .utf8)!
        let decoder = JSONDecoder()
        
        do {
            let status = try decoder.decode(DaemonStatus.self, from: data)
            XCTAssertTrue(status.isRunning)
            XCTAssertEqual(status.pid, 12345)
            XCTAssertEqual(status.logPath, "/var/log/always.log")
        } catch {
            XCTFail("Failed to decode DaemonStatus: \(error)")
        }
    }
}
