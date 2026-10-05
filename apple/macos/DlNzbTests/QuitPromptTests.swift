import Testing

@testable import DlNzbApp

@Suite("Quit prompt")
struct QuitPromptTests {
  @Test("Nothing unfinished quits without asking")
  func nothingUnfinished() {
    #expect(QuitPrompt(unfinished: 0) == nil)
  }

  @Test("One download reads in the singular, per the SPEC")
  func singular() throws {
    let prompt = try #require(QuitPrompt(unfinished: 1))
    #expect(prompt.title == "Quit dl-nzb?")
    #expect(prompt.message == "1 download will continue next time you open dl-nzb.")
    #expect(prompt.confirm == "Quit")
    #expect(prompt.cancel == "Cancel")
  }

  @Test("Several downloads read in the plural")
  func plural() throws {
    let prompt = try #require(QuitPrompt(unfinished: 3))
    #expect(prompt.message == "3 downloads will continue next time you open dl-nzb.")
  }
}
