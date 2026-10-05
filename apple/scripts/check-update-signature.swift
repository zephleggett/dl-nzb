// Check a Sparkle EdDSA signature the way an installed copy will, against the
// public key the app carries (SUPublicEDKey):
//
//   xcrun swift apple/scripts/check-update-signature.swift <public key> <signature> <file>
//
// Both are base64, as generate_appcast writes the signature and generate_keys
// prints the key. make-appcast.sh runs it on the DMG it has just signed.
import CryptoKit
import Foundation

let arguments = CommandLine.arguments
guard arguments.count == 4, let keyData = Data(base64Encoded: arguments[1]), let signature = Data(base64Encoded: arguments[2]) else {
  FileHandle.standardError.write(Data("usage: check-update-signature.swift <public key> <signature> <file>\n".utf8))
  exit(2)
}
do {
  let key = try Curve25519.Signing.PublicKey(rawRepresentation: keyData)
  let file = try Data(contentsOf: URL(fileURLWithPath: arguments[3]), options: .alwaysMapped)
  guard key.isValidSignature(signature, for: file) else {
    FileHandle.standardError.write(Data("error: the signature does not match the app's SUPublicEDKey; is the private key its other half?\n".utf8))
    exit(1)
  }
  print("the signature matches the app's SUPublicEDKey")
} catch {
  FileHandle.standardError.write(Data("error: \(error)\n".utf8))
  exit(1)
}
