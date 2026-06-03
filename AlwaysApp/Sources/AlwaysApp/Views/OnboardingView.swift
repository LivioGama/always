import SwiftUI
import Security

struct OnboardingView: View {
    @State private var apiKey: String = ""
    @State private var isSaving = false
    @State private var saveError: String?
    @State private var showSuccess = false
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openWindow) private var openWindow
    
    private let service = "com.always.daemon"
    private let account = "groq_api_key"
    
    var body: some View {
        VStack(spacing: 24) {
            // Header
            VStack(spacing: 8) {
                Image(systemName: "mic.fill")
                    .font(.system(size: 48))
                    .foregroundColor(.accentColor)
                
                Text("Welcome to Always")
                    .font(.title)
                    .fontWeight(.bold)
                
                Text("Set up your Groq API key to get started")
                    .font(.subheadline)
                    .foregroundColor(.secondary)
            }
            .padding(.top, 20)
            
            // API Key Input
            VStack(alignment: .leading, spacing: 8) {
                Text("Groq API Key")
                    .font(.headline)
                
                SecureField("Enter your API key", text: $apiKey)
                    .textFieldStyle(.roundedBorder)
                    .controlSize(.large)
                
                Text("Your key will be stored securely in the macOS Keychain")
                    .font(.caption)
                    .foregroundColor(.secondary)
            }
            .padding(.horizontal, 20)
            
            // Error message
            if let error = saveError {
                Text(error)
                    .font(.caption)
                    .foregroundColor(.red)
                    .padding(.horizontal, 20)
            }
            
            // Success message
            if showSuccess {
                Text("API key saved successfully!")
                    .font(.caption)
                    .foregroundColor(.green)
                    .padding(.horizontal, 20)
            }
            
            Spacer()
            
            // Action buttons
            VStack(spacing: 12) {
                Button(action: saveAPIKey) {
                    if isSaving {
                        ProgressView()
                            .progressViewStyle(.circular)
                    } else {
                        Text("Save API Key")
                            .fontWeight(.semibold)
                    }
                }
                .disabled(apiKey.isEmpty || isSaving)
                .buttonStyle(.borderedProminent)
                .controlSize(.large)
                
                Button("Get an API key") {
                    if let url = URL(string: "https://console.groq.com/keys") {
                        NSWorkspace.shared.open(url)
                    }
                }
                .buttonStyle(.bordered)
            }
            .padding(.bottom, 20)
        }
        .frame(width: 500, height: 400)
        .onAppear {
            loadExistingKey()
        }
    }
    
    private func loadExistingKey() {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne
        ]
        
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        
        if status == errSecSuccess, let data = item as? Data,
           let key = String(data: data, encoding: .utf8) {
            apiKey = key
        }
    }
    
    private func saveAPIKey() {
        isSaving = true
        saveError = nil
        
        // First, delete any existing key
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account
        ]
        SecItemDelete(deleteQuery as CFDictionary)
        
        // Add the new key
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecValueData as String: apiKey.data(using: .utf8)!
        ]
        
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            isSaving = false
            
            if status == errSecSuccess {
                showSuccess = true
                DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
                    dismiss()
                }
            } else {
                saveError = "Failed to save API key: \(status.description)"
            }
        }
    }
}

#Preview {
    OnboardingView()
}
