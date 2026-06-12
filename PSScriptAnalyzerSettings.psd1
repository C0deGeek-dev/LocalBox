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
    # Quarantined legacy debt (tracked for the wizard/launcher extraction
    # work, where each site gets reviewed):
    #   PSAvoidAssignmentToAutomaticVariable, PSAvoidUsingEmptyCatchBlock,
    #   PSReviewUnusedParameter
    ExcludeRules = @(
        'PSAvoidUsingWriteHost',
        'PSUseShouldProcessForStateChangingFunctions',
        'PSUseSingularNouns',
        'PSUseApprovedVerbs',
        'PSUseOutputTypeCorrectly',
        'PSUseBOMForUnicodeEncodedFile',
        'PSAvoidAssignmentToAutomaticVariable',
        'PSAvoidUsingEmptyCatchBlock',
        'PSReviewUnusedParameter'
    )
}
