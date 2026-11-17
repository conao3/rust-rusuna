import Audit.Generators

open model

#assay quoteChars using unquote_quote unquote_quoteChars
#assay quoteStringChars using unquote_quoteString unquote_quoteStringChars
#assay assignStep assign tokValue using slots_numbered
#assay lateralScan using lateralScan_limit
#assay distinct aliasValid selectionValid relationValid using distinct_iff aliasValid_iff selectionValid_iff relationValid_iff

#assay scalarValid membershipValid using scalarValid_iff membershipValid_iff
