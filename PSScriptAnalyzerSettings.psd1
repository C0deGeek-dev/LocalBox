@{
    # Curated gate: everything not excluded below is blocking in CI.
    # Point fixes and per-function SuppressMessage attributes (with written
    # justification) are preferred over new exclusions.
    #
    # Excluded by design (the launcher is an interactive CLI tool):
    #   PSAvoidUsingWriteHost            - console output is the product surface
    #   PSUseShouldProcessForStateChangingFunctions - Start-*/Stop-*/Set-* manage
    #                                      tool-owned processes and state files
    # Excluded as shipped-API compatibility (renames would break callers):
    #   PSUseSingularNouns, PSUseApprovedVerbs
    # Excluded as informational noise for this codebase:
    #   PSUseOutputTypeCorrectly
    # Excluded as repo policy (files are UTF-8 without BOM; PowerShell 7+):
    #   PSUseBOMForUnicodeEncodedFile
    # The former quarantined-debt rules (PSAvoidUsingEmptyCatchBlock,
    # PSAvoidAssignmentToAutomaticVariable, PSReviewUnusedParameter) are now
    # blocking: every legacy site was fixed or carries a narrowly scoped,
    # justified per-function SuppressMessage (presence probes, the shipped
    # -Profile parameter name, closure-consumed switches).
    ExcludeRules = @(
        'PSAvoidUsingWriteHost',
        'PSUseShouldProcessForStateChangingFunctions',
        'PSUseSingularNouns',
        'PSUseApprovedVerbs',
        'PSUseOutputTypeCorrectly',
        'PSUseBOMForUnicodeEncodedFile'
    )
}
